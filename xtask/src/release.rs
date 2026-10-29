//! Release assembly, run by the `release` workflow on the collected packages:
//!
//! - `cargo xtask release manifest <dir> --repo <owner/name> --date <YYYY-MM-DD>
//!   [--minimum-version <X.Y.Z>]` checks that `<dir>` holds only release files of the current
//!   version, checks each `.sha256` file against its package and removes it, and writes
//!   `SHA256SUMS.txt` and the self-update manifest `latest.json` (checked with the launcher's own
//!   parser);
//! - `cargo xtask release sign <dir>` signs `latest.json` with the minisign secret key in
//!   `VL_UPDATE_SECRET_KEY` (its password, when it has one, in `VL_UPDATE_SECRET_KEY_PASSWORD`)
//!   into `latest.json.minisig` and verifies the signature with the public key of that secret key
//!   and, when set, with `VL_UPDATE_PUBLIC_KEY`.
//!
//! Release file names: `vibelauncher-<version>-<platform>.<ext>`, see [`classify`].

use std::fs;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};
use vl_update::{Manifest, UpdateKey, verify_manifest};

use crate::read_version;

/// The checksum list of the release.
pub const SUMS: &str = "SHA256SUMS.txt";
/// The self-update manifest.
pub const MANIFEST: &str = "latest.json";
/// Its detached minisign signature.
pub const SIGNATURE: &str = "latest.json.minisig";

/// A release file: the manifest entries it gives (`os`, `arch`, `kind`), none for a file the
/// self-update does not install (the `.zsync` file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFile {
    pub name: String,
    pub entries: Vec<(&'static str, &'static str, &'static str)>,
}

/// What a release file named `name` is, for version `version`; `None` when the name is not a
/// release file name.
///
/// | Name | Manifest entries |
/// |---|---|
/// | `-windows-<x64\|arm64>.msi` | windows, msi |
/// | `-windows-<x64\|arm64>-portable.zip` | windows, portable |
/// | `-macos-universal.dmg` | mac_os, dmg, for x86_64 and aarch64 |
/// | `-linux-<x86_64\|aarch64>.tar.gz` | linux, portable |
/// | `-linux-<arch>.AppImage` | linux, app_image |
/// | `-linux-<arch>.AppImage.zsync` | — |
/// | `-linux-<arch>.deb`, `.rpm` | linux, deb or rpm |
pub fn classify(name: &str, version: &str) -> Option<ReleaseFile> {
    let rest = name.strip_prefix(&format!("vibelauncher-{version}-"))?;
    let windows_arch = |a: &str| match a {
        "x64" => Some("x86_64"),
        "arm64" => Some("aarch64"),
        _ => None,
    };
    let linux_arch = |a: &str| match a {
        "x86_64" => Some("x86_64"),
        "aarch64" => Some("aarch64"),
        _ => None,
    };
    let windows = |rest: &str| {
        let (arch, kind) = match rest.strip_suffix("-portable.zip") {
            Some(arch) => (arch, "portable"),
            None => (rest.strip_suffix(".msi")?, "msi"),
        };
        Some(vec![("windows", windows_arch(arch)?, kind)])
    };
    let linux = |rest: &str| {
        let (arch, ext) = rest.split_once('.')?;
        let arch = linux_arch(arch)?;
        let kind = match ext {
            "tar.gz" => "portable",
            "AppImage" => "app_image",
            "AppImage.zsync" => return Some(Vec::new()),
            "deb" => "deb",
            "rpm" => "rpm",
            _ => return None,
        };
        Some(vec![("linux", arch, kind)])
    };
    let entries = match rest {
        "macos-universal.dmg" => vec![("mac_os", "x86_64", "dmg"), ("mac_os", "aarch64", "dmg")],
        _ => match (rest.strip_prefix("windows-"), rest.strip_prefix("linux-")) {
            (Some(rest), _) => windows(rest)?,
            (_, Some(rest)) => linux(rest)?,
            _ => return None,
        },
    };
    Some(ReleaseFile {
        name: name.to_owned(),
        entries,
    })
}

/// Options of `release manifest`.
pub struct ManifestOptions<'a> {
    /// `owner/name` of the GitHub repository the release belongs to.
    pub repo: &'a str,
    /// The release date, `YYYY-MM-DD`.
    pub date: &'a str,
    /// The oldest version that may update itself to this release.
    pub minimum_version: &'a str,
}

/// `cargo xtask release manifest`.
pub fn manifest(dir: &Path, options: &ManifestOptions) -> Result<(), String> {
    let version = read_version()?;
    let mut files: Vec<(ReleaseFile, Vec<u8>)> = Vec::new();
    let mut sidecars: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err(format!("{}: not a file", entry.path().display()));
        }
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    for name in names {
        if name.ends_with(".sha256") {
            sidecars.push(name);
            continue;
        }
        if [SUMS, MANIFEST, SIGNATURE].contains(&name.as_str()) {
            return Err(format!("{name} is already in {}", dir.display()));
        }
        let file = classify(&name, &version)
            .ok_or_else(|| format!("{name} is not a release file of version {version}"))?;
        let bytes = fs::read(dir.join(&name)).map_err(|e| format!("{name}: {e}"))?;
        files.push((file, bytes));
    }
    if files.is_empty() {
        return Err(format!("no release files in {}", dir.display()));
    }
    let sums: Vec<(String, String)> = files
        .iter()
        .map(|(f, bytes)| (f.name.clone(), hex(&Sha256::digest(bytes))))
        .collect();
    // Each package's own checksum file must agree; then it is not needed any more.
    for sidecar in &sidecars {
        let text = fs::read_to_string(dir.join(sidecar)).map_err(|e| format!("{sidecar}: {e}"))?;
        let package = sidecar.trim_end_matches(".sha256");
        let expected = sums
            .iter()
            .find(|(n, _)| n == package)
            .map(|(_, s)| s)
            .ok_or_else(|| format!("{sidecar}: no file {package}"))?;
        if text.split_whitespace().next() != Some(expected.as_str()) {
            return Err(format!("{sidecar} does not match {package}"));
        }
    }
    for sidecar in &sidecars {
        fs::remove_file(dir.join(sidecar)).map_err(|e| format!("{sidecar}: {e}"))?;
    }
    let sums_text = sums_file(&sums);
    let tagged: Vec<(&ReleaseFile, u64, &str)> = files
        .iter()
        .zip(&sums)
        .map(|((f, bytes), (_, sha))| (f, bytes.len() as u64, sha.as_str()))
        .collect();
    let manifest = manifest_json(&version, options, &tagged)?;
    write(&dir.join(SUMS), sums_text.as_bytes())?;
    write(&dir.join(MANIFEST), manifest.as_bytes())?;
    println!(
        "{}: {} files, {SUMS}, {MANIFEST} ({} artifacts)",
        dir.display(),
        files.len(),
        Manifest::parse(manifest.as_bytes())
            .map_err(|e| e.to_string())?
            .artifacts
            .len()
    );
    print!("{sums_text}");
    Ok(())
}

/// `<sha256>  <name>` lines in name order, as `sha256sum` writes and checks them.
fn sums_file(sums: &[(String, String)]) -> String {
    let mut lines: Vec<String> = sums.iter().map(|(n, s)| format!("{s}  {n}\n")).collect();
    lines.sort_by(|a, b| a[66..].cmp(&b[66..]));
    lines.concat()
}

/// The manifest text, checked with the launcher's parser.
fn manifest_json(
    version: &str,
    options: &ManifestOptions,
    files: &[(&ReleaseFile, u64, &str)],
) -> Result<String, String> {
    let repo = options.repo;
    if repo.split('/').count() != 2 || repo.contains(char::is_whitespace) {
        return Err(format!("`{repo}` is not owner/name"));
    }
    let tag = format!("v{version}");
    let download = format!("https://github.com/{repo}/releases/download/{tag}");
    let download = download.as_str();
    let artifacts: Vec<ArtifactOut> = files
        .iter()
        .flat_map(|(file, size, sha)| {
            file.entries
                .iter()
                .map(move |&(os, arch, kind)| ArtifactOut {
                    os,
                    arch,
                    kind,
                    url: format!("{download}/{}", file.name),
                    sha256: sha,
                    size: *size,
                })
        })
        .collect();
    let manifest = ManifestOut {
        schema: vl_update::manifest::SCHEMA,
        channel: "stable",
        version,
        date: options.date,
        minimum_version: options.minimum_version,
        page: format!("https://github.com/{repo}/releases/tag/{tag}"),
        artifacts: &artifacts,
    };
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())? + "\n";
    let parsed = Manifest::parse(text.as_bytes()).map_err(|e| format!("{MANIFEST}: {e}"))?;
    if parsed.artifacts.len() != artifacts.len() {
        return Err(format!(
            "{MANIFEST}: the launcher reads {} of {} artifacts",
            parsed.artifacts.len(),
            artifacts.len()
        ));
    }
    Ok(text)
}

/// `latest.json` as written, in the field order of the format.
#[derive(Serialize)]
struct ManifestOut<'a> {
    schema: u32,
    channel: &'a str,
    version: &'a str,
    date: &'a str,
    minimum_version: &'a str,
    page: String,
    artifacts: &'a [ArtifactOut<'a>],
}

#[derive(Serialize)]
struct ArtifactOut<'a> {
    os: &'a str,
    arch: &'a str,
    kind: &'a str,
    url: String,
    sha256: &'a str,
    size: u64,
}

/// `cargo xtask release sign`.
pub fn sign(dir: &Path) -> Result<(), String> {
    let secret = std::env::var("VL_UPDATE_SECRET_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or("VL_UPDATE_SECRET_KEY is not set")?;
    let password = std::env::var("VL_UPDATE_SECRET_KEY_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty());
    let public = std::env::var("VL_UPDATE_PUBLIC_KEY")
        .ok()
        .filter(|p| !p.trim().is_empty());
    let manifest = fs::read(dir.join(MANIFEST)).map_err(|e| format!("{MANIFEST}: {e}"))?;
    let (signature, key) = sign_bytes(&manifest, &secret, password)?;
    verify_manifest(&manifest, signature.as_bytes(), &key)
        .map_err(|e| format!("the new signature does not verify: {e}"))?;
    if let Some(public) = public {
        let release =
            UpdateKey::from_text(&public).map_err(|e| format!("VL_UPDATE_PUBLIC_KEY: {e}"))?;
        verify_manifest(&manifest, signature.as_bytes(), &release)
            .map_err(|e| format!("the signature does not verify with VL_UPDATE_PUBLIC_KEY: {e}"))?;
    }
    write(&dir.join(SIGNATURE), signature.as_bytes())?;
    println!("{}: signed", dir.join(SIGNATURE).display());
    Ok(())
}

/// A prehashed minisign signature of `data` with the secret key file text `secret`, and the
/// public key of that secret key.
fn sign_bytes(
    data: &[u8],
    secret: &str,
    password: Option<String>,
) -> Result<(String, UpdateKey), String> {
    let boxed = minisign::SecretKeyBox::from_string(secret.trim())
        .map_err(|e| format!("VL_UPDATE_SECRET_KEY: {e}"))?;
    // Never `into_secret_key(None)`: it would ask for the password on the terminal.
    let sk = match password {
        Some(password) => boxed.into_secret_key(Some(password)),
        None => boxed.into_unencrypted_secret_key(),
    }
    .map_err(|e| format!("VL_UPDATE_SECRET_KEY: {e}"))?;
    let pk = minisign::PublicKey::from_secret_key(&sk)
        .map_err(|e| format!("VL_UPDATE_SECRET_KEY: {e}"))?;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let trusted = format!("timestamp:{time}\tfile:{MANIFEST}");
    let signature = minisign::sign(
        Some(&pk),
        &sk,
        std::io::Cursor::new(data),
        Some(&trusted),
        Some("signature of the VibeLauncher release manifest"),
    )
    .map_err(|e| format!("signing failed: {e}"))?;
    let key = UpdateKey::from_text(&pk.to_base64()).map_err(|e| e.to_string())?;
    Ok((signature.into_string(), key))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
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

    #[test]
    fn release_file_names() {
        let v = "1.2.3";
        let entries = |name: &str| classify(name, v).map(|f| f.entries);
        assert_eq!(
            entries("vibelauncher-1.2.3-windows-x64-portable.zip"),
            Some(vec![("windows", "x86_64", "portable")])
        );
        assert_eq!(
            entries("vibelauncher-1.2.3-windows-x64.msi"),
            Some(vec![("windows", "x86_64", "msi")])
        );
        assert_eq!(
            entries("vibelauncher-1.2.3-macos-universal.dmg"),
            Some(vec![
                ("mac_os", "x86_64", "dmg"),
                ("mac_os", "aarch64", "dmg")
            ])
        );
        assert_eq!(
            entries("vibelauncher-1.2.3-linux-aarch64.tar.gz"),
            Some(vec![("linux", "aarch64", "portable")])
        );
        assert_eq!(
            entries("vibelauncher-1.2.3-linux-x86_64.AppImage"),
            Some(vec![("linux", "x86_64", "app_image")])
        );
        assert_eq!(
            entries("vibelauncher-1.2.3-linux-x86_64.AppImage.zsync"),
            Some(vec![])
        );
        assert_eq!(
            entries("vibelauncher-1.2.3-linux-x86_64.rpm"),
            Some(vec![("linux", "x86_64", "rpm")])
        );
        for name in [
            "vibelauncher-1.2.4-linux-x86_64.deb",
            "vibelauncher-1.2.3-linux-riscv64.deb",
            "vibelauncher-1.2.3-linux-x86_64.zip",
            "vibelauncher-1.2.3-windows-x64.exe",
            "vibelauncher-1.2.3-macos-arm64.dmg",
            "notes.txt",
        ] {
            assert_eq!(classify(name, v), None, "{name}");
        }
    }

    fn options() -> ManifestOptions<'static> {
        ManifestOptions {
            repo: "example/launcher",
            date: "2026-11-02",
            minimum_version: "0.1.0",
        }
    }

    #[test]
    fn the_manifest_lists_every_install_method_and_the_launcher_reads_it() {
        let names = [
            "vibelauncher-1.0.0-windows-x64-portable.zip",
            "vibelauncher-1.0.0-macos-universal.dmg",
            "vibelauncher-1.0.0-linux-x86_64.AppImage",
            "vibelauncher-1.0.0-linux-x86_64.AppImage.zsync",
            "vibelauncher-1.0.0-linux-x86_64.tar.gz",
            "vibelauncher-1.0.0-linux-x86_64.deb",
        ];
        let files: Vec<ReleaseFile> = names
            .iter()
            .map(|n| classify(n, "1.0.0").unwrap())
            .collect();
        let sha = "ab".repeat(32);
        let tagged: Vec<(&ReleaseFile, u64, &str)> =
            files.iter().map(|f| (f, 1000, sha.as_str())).collect();
        let text = manifest_json("1.0.0", &options(), &tagged).unwrap();
        let manifest = Manifest::parse(text.as_bytes()).unwrap();
        assert_eq!(manifest.artifacts.len(), 6);
        assert_eq!(
            manifest.page,
            "https://github.com/example/launcher/releases/tag/v1.0.0"
        );
        assert!(manifest.artifacts.iter().all(|a| a.url.starts_with(
            "https://github.com/example/launcher/releases/download/v1.0.0/vibelauncher-1.0.0-"
        )));
        // A pre-release is not a stable release.
        assert!(manifest_json("1.0.0-rc.1", &options(), &tagged).is_err());
    }

    #[test]
    fn sums_are_sorted_by_name() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let text = sums_file(&[("z.deb".into(), a.clone()), ("a.rpm".into(), b.clone())]);
        assert_eq!(text, format!("{b}  a.rpm\n{a}  z.deb\n"));
    }

    #[test]
    fn signs_with_a_plain_and_a_password_protected_key() {
        let data = b"{\"schema\":1}";
        let plain = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let text = plain.sk.to_box(None).unwrap().into_string();
        let (sig, key) = sign_bytes(data, &text, None).unwrap();
        key.verify(data, sig.as_bytes()).unwrap();
        assert_eq!(
            key,
            UpdateKey::from_text(&plain.pk.to_base64()).unwrap(),
            "the public key comes from the secret key"
        );

        let locked = minisign::KeyPair::generate_encrypted_keypair(Some("pass".into())).unwrap();
        let text = locked.sk.to_box(None).unwrap().into_string();
        let (sig, key) = sign_bytes(data, &text, Some("pass".into())).unwrap();
        key.verify(data, sig.as_bytes()).unwrap();
        assert!(sign_bytes(data, "not a key", None).is_err());
    }
}
