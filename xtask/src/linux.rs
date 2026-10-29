//! `cargo xtask package <tar|appimage|deb|rpm|linux> [--target <triple>]`: the Linux packages.
//!
//! Builds `vibelauncher` and `vl-cli` in release mode (symbols stripped) for a Linux GNU target and
//! writes into `target/dist`, each with a `.sha256` file next to it:
//! - `vibelauncher-<version>-linux-<arch>.tar.gz`: a folder `vibelauncher-<version>-linux-<arch>`
//!   with both executables, the portable marker and the license;
//! - `vibelauncher-<version>-linux-<arch>.AppImage` and its `.zsync` file (`appimagetool`, with
//!   the type 2 runtime named by `APPIMAGETOOL_RUNTIME_FILE`, and `zsyncmake`);
//! - `vibelauncher-<version>-linux-<arch>.deb` and `.rpm` (`nfpm`): the executables in
//!   `/usr/lib/vibelauncher` with the install marker, links in `/usr/bin`, the desktop entry,
//!   the icons and the AppStream metadata.
//!
//! `linux` builds all four. The tools are taken from `PATH`, or from the `APPIMAGETOOL` and `NFPM`
//! variables.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};

use crate::package::{
    BINARIES, PORTABLE_MARKER, build_release, host_target, read, write_with_sums,
};
use crate::{read_version, root};

/// The application ID: the desktop entry, the icon, the AppStream component and the Wayland app
/// ID of the desktop app.
pub const APP_ID: &str = "me.vibefork.VibeLauncher";
/// The package name of the deb and rpm packages.
const PACKAGE: &str = "vibelauncher";
/// The GitHub repository whose releases carry the AppImage and its `.zsync` file.
const REPOSITORY: (&str, &str) = ("FQ401", "vibelauncher");
/// Where the deb and rpm packages put the executables and the install marker.
const LIB_DIR: &str = "/usr/lib/vibelauncher";
/// `vl_update::INSTALL_MARKER`: names the install method next to the executables.
const INSTALL_MARKER: &str = "vibelauncher.install";
/// The time of every entry of the tar archive: 1980-01-01, as in the zip archives.
const ENTRY_TIME: u64 = 315_532_800;
const MAINTAINER: &str = "VibeLauncher contributors <vibelauncher@vibefork.me>";
const HOMEPAGE: &str = "https://github.com/FQ401/vibelauncher";
const SUMMARY: &str = "Minecraft launcher with mods and modpacks";
const DESCRIPTION: &str = "VibeLauncher installs, manages and launches Minecraft: Java Edition: \
instances with their own game version, mod loader, mods and modpacks, Microsoft account \
sign-in and Java runtimes. Not an official Minecraft product.";

/// A Linux package format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Tar,
    AppImage,
    Deb,
    Rpm,
}

/// The formats a `cargo xtask package <name>` builds; `None` for a name that is not a Linux one.
pub fn formats(name: &str) -> Option<Vec<Format>> {
    Some(match name {
        "tar" => vec![Format::Tar],
        "appimage" => vec![Format::AppImage],
        "deb" => vec![Format::Deb],
        "rpm" => vec![Format::Rpm],
        "linux" => vec![Format::Tar, Format::AppImage, Format::Deb, Format::Rpm],
        _ => return None,
    })
}

/// The architecture names of a Linux target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arch {
    /// In file names, the AppImage and rpm: `x86_64`, `aarch64`.
    name: &'static str,
    /// For nfpm and deb: `amd64`, `arm64`.
    deb: &'static str,
}

fn linux_arch(target: &str) -> Result<Arch, String> {
    match target {
        "x86_64-unknown-linux-gnu" => Ok(Arch {
            name: "x86_64",
            deb: "amd64",
        }),
        "aarch64-unknown-linux-gnu" => Ok(Arch {
            name: "aarch64",
            deb: "arm64",
        }),
        _ => Err(format!(
            "the Linux packages are built for a Linux GNU target, not `{target}` \
             (pass --target x86_64-unknown-linux-gnu or aarch64-unknown-linux-gnu)"
        )),
    }
}

/// `vibelauncher-<version>-linux-<arch>`, the stem of every file name.
fn stem(version: &str, arch: &str) -> String {
    format!("vibelauncher-{version}-linux-{arch}")
}

/// The AppImage update information: the newest `.zsync` file of the GitHub releases.
fn update_information(arch: &str) -> String {
    let (owner, repo) = REPOSITORY;
    format!("gh-releases-zsync|{owner}|{repo}|latest|vibelauncher-*-linux-{arch}.AppImage.zsync")
}

/// One file of a package: its path, content and Unix mode.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: String,
    content: Vec<u8>,
    mode: u32,
}

impl Entry {
    fn file(path: impl Into<String>, content: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            content,
            mode: 0o644,
        }
    }

    fn executable(path: impl Into<String>, content: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            content,
            mode: 0o755,
        }
    }
}

/// What every package is made of.
struct Inputs {
    version: String,
    arch: Arch,
    /// `(name, content)` of each executable.
    binaries: Vec<(&'static str, Vec<u8>)>,
    /// `(size, PNG)` of each icon, smallest first.
    icons: Vec<(u32, Vec<u8>)>,
    desktop: Vec<u8>,
    metainfo: Vec<u8>,
    app_run: Vec<u8>,
    license: Vec<u8>,
}

/// Builds `formats` for `target` (the host when `None`).
pub fn package(formats: &[Format], target: Option<&str>) -> Result<(), String> {
    let target = match target {
        Some(t) => t.to_owned(),
        None => host_target()?,
    };
    let arch = linux_arch(&target)?;
    let runtime = if formats.contains(&Format::AppImage) {
        let runtime = std::env::var_os("APPIMAGETOOL_RUNTIME_FILE")
            .map(PathBuf::from)
            .ok_or("the AppImage needs APPIMAGETOOL_RUNTIME_FILE: a pinned type 2 runtime")?;
        if !runtime.is_file() {
            return Err(format!("{}: no such file", runtime.display()));
        }
        Some(runtime)
    } else {
        None
    };
    let version = read_version()?;
    build_release(&target, true)?;

    let release = root().join("target").join(&target).join("release");
    let mut binaries = Vec::new();
    for (_, bin) in BINARIES {
        binaries.push((bin, read(&release.join(bin))?));
    }
    let linux = root().join("packaging").join("linux");
    let text = |name: &str| read(&linux.join(name)).map(|b| lf(&b));
    let inputs = Inputs {
        version,
        arch,
        binaries,
        icons: ico_pngs(&read(&root().join("assets/icons/vibelauncher.ico"))?)?,
        desktop: text(&format!("{APP_ID}.desktop"))?,
        metainfo: text(&format!("{APP_ID}.metainfo.xml"))?,
        app_run: text("AppRun")?,
        license: read(&root().join("LICENSE"))?,
    };

    let dist = root().join("target").join("dist");
    let staging = dist.join(format!("staging-linux-{}", arch.name));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    for format in formats {
        match format {
            Format::Tar => {
                let name = format!("{}.tar.gz", stem(&inputs.version, arch.name));
                write_with_sums(&dist, &name, &tar_gz(&tar_entries(&inputs))?)?;
            }
            Format::AppImage => {
                let runtime = runtime.as_deref().ok_or("no AppImage runtime")?;
                app_image(&inputs, &staging.join("AppDir"), &dist, runtime)?;
            }
            Format::Deb | Format::Rpm => {
                let packager = if *format == Format::Deb { "deb" } else { "rpm" };
                system_package(&inputs, packager, &staging.join("root"), &dist)?;
            }
        }
    }
    Ok(())
}

/// The tar archive: a folder with both executables, the portable marker and the license.
fn tar_entries(inputs: &Inputs) -> Vec<Entry> {
    let dir = stem(&inputs.version, inputs.arch.name);
    let mut entries: Vec<Entry> = inputs
        .binaries
        .iter()
        .map(|(name, content)| Entry::executable(format!("{dir}/{name}"), content.clone()))
        .collect();
    entries.push(Entry::file(format!("{dir}/{PORTABLE_MARKER}"), Vec::new()));
    entries.push(Entry::file(
        format!("{dir}/LICENSE.txt"),
        inputs.license.clone(),
    ));
    entries
}

/// The desktop entry, the icons and the AppStream metadata below `usr/share`; the metadata file
/// is `<id>.<metainfo>`.
fn share_entries(inputs: &Inputs, metainfo: &str) -> Vec<Entry> {
    let mut entries = vec![Entry::file(
        format!("usr/share/applications/{APP_ID}.desktop"),
        inputs.desktop.clone(),
    )];
    for (size, png) in &inputs.icons {
        entries.push(Entry::file(
            format!("usr/share/icons/hicolor/{size}x{size}/apps/{APP_ID}.png"),
            png.clone(),
        ));
    }
    entries.push(Entry::file(
        format!("usr/share/metainfo/{APP_ID}.{metainfo}"),
        inputs.metainfo.clone(),
    ));
    entries
}

/// The AppDir: `AppRun`, the desktop entry and the largest icon at its root, the executables in
/// `usr/bin` and the shared files (the metadata named `.appdata.xml`, as appimagetool reads it).
fn app_dir_entries(inputs: &Inputs) -> Result<Vec<Entry>, String> {
    let (_, icon) = inputs.icons.last().ok_or("no icons")?;
    let mut entries = vec![
        Entry::executable("AppRun", inputs.app_run.clone()),
        Entry::file(format!("{APP_ID}.desktop"), inputs.desktop.clone()),
        Entry::file(format!("{APP_ID}.png"), icon.clone()),
    ];
    for (name, content) in &inputs.binaries {
        entries.push(Entry::executable(
            format!("usr/bin/{name}"),
            content.clone(),
        ));
    }
    entries.extend(share_entries(inputs, "appdata.xml"));
    entries.push(Entry::file(
        format!("usr/share/licenses/{PACKAGE}/LICENSE"),
        inputs.license.clone(),
    ));
    Ok(entries)
}

fn app_image(inputs: &Inputs, app_dir: &Path, dist: &Path, runtime: &Path) -> Result<(), String> {
    write_tree(app_dir, &app_dir_entries(inputs)?)?;
    let arch = inputs.arch.name;
    let name = format!("{}.AppImage", stem(&inputs.version, arch));
    let zsync = format!("{name}.zsync");
    for file in [&name, &zsync] {
        let path = dist.join(file);
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let tool = std::env::var("APPIMAGETOOL").unwrap_or_else(|_| "appimagetool".into());
    let runtime = runtime.to_string_lossy();
    let app_dir = app_dir.to_string_lossy();
    let update = update_information(arch);
    let args = [
        "--runtime-file",
        &runtime,
        "--updateinformation",
        &update,
        &app_dir,
        &name,
    ];
    println!("== appimage: {tool} {}", args.join(" "));
    // appimagetool writes the `.zsync` file (through zsyncmake) into its working folder.
    let status = Command::new(&tool)
        .args(args)
        .env("ARCH", arch)
        .current_dir(dist)
        .status()
        .map_err(|e| format!("appimage: cannot run {tool}: {e}"))?;
    if !status.success() {
        return Err(format!("appimage failed ({status})"));
    }
    if !dist.join(&zsync).is_file() {
        return Err(format!("appimage: no {zsync} (is zsyncmake installed?)"));
    }
    let bytes = read(&dist.join(&name))?;
    write_with_sums(dist, &name, &bytes)?;
    Ok(())
}

/// The files of the deb and rpm packages as `(path below /, entry)`; the install marker and the
/// license location differ by `packager`.
fn system_entries(inputs: &Inputs, packager: &str) -> Vec<Entry> {
    let lib = LIB_DIR.trim_start_matches('/');
    let mut entries: Vec<Entry> = inputs
        .binaries
        .iter()
        .map(|(name, content)| Entry::executable(format!("{lib}/{name}"), content.clone()))
        .collect();
    entries.push(Entry::file(
        format!("{lib}/{INSTALL_MARKER}"),
        format!("{packager}\n").into_bytes(),
    ));
    entries.extend(share_entries(inputs, "metainfo.xml"));
    let license = if packager == "deb" {
        format!("usr/share/doc/{PACKAGE}/copyright")
    } else {
        format!("usr/share/licenses/{PACKAGE}/LICENSE")
    };
    entries.push(Entry::file(license, inputs.license.clone()));
    entries
}

/// The nfpm configuration (JSON, which nfpm reads as YAML) for the files staged in `staged`.
fn nfpm_config(inputs: &Inputs, packager: &str, entries: &[Entry], staged: &Path) -> Value {
    let mut contents: Vec<Value> = Vec::new();
    // The package owns its own folders, so removing it removes them.
    for dir in [LIB_DIR.to_owned(), doc_dir(packager)] {
        contents.push(json!({ "dst": dir, "type": "dir", "file_info": { "mode": 0o755 } }));
    }
    for entry in entries {
        contents.push(json!({
            "src": staged.join(&entry.path).to_string_lossy(),
            "dst": format!("/{}", entry.path),
            "file_info": { "mode": entry.mode },
        }));
    }
    for (_, bin) in BINARIES {
        contents.push(json!({
            "src": format!("{LIB_DIR}/{bin}"),
            "dst": format!("/usr/bin/{bin}"),
            "type": "symlink",
        }));
    }
    json!({
        "name": PACKAGE,
        "arch": inputs.arch.deb,
        "platform": "linux",
        "version": inputs.version,
        "version_schema": "semver",
        "section": "games",
        "priority": "optional",
        "maintainer": MAINTAINER,
        "vendor": "VibeLauncher contributors",
        "homepage": HOMEPAGE,
        "license": "MIT",
        "description": format!("{SUMMARY}\n{DESCRIPTION}"),
        "contents": contents,
        "overrides": {
            "deb": {
                "depends": ["libc6 (>= 2.35)", "libgcc-s1"],
                "recommends": [
                    "libvulkan1", "libegl1", "libwayland-client0", "libwayland-cursor0",
                    "libxkbcommon0", "libxkbcommon-x11-0", "libx11-6", "libxcursor1",
                    "libxrandr2", "libxi6",
                ],
            },
            "rpm": {
                "depends": ["glibc >= 2.35", "libgcc"],
                "recommends": [
                    "vulkan-loader", "libglvnd-egl", "libwayland-client", "libwayland-cursor",
                    "libxkbcommon", "libxkbcommon-x11", "libX11", "libXcursor", "libXrandr",
                    "libXi",
                ],
            },
        },
    })
}

/// The package's own documentation folder: the copyright file (deb) or the license (rpm).
fn doc_dir(packager: &str) -> String {
    if packager == "deb" {
        format!("/usr/share/doc/{PACKAGE}")
    } else {
        format!("/usr/share/licenses/{PACKAGE}")
    }
}

fn system_package(
    inputs: &Inputs,
    packager: &str,
    staging: &Path,
    dist: &Path,
) -> Result<(), String> {
    let staged = staging.join(packager);
    let entries = system_entries(inputs, packager);
    write_tree(&staged, &entries)?;
    let config = staging.join(format!("nfpm-{packager}.yaml"));
    let text = serde_json::to_string_pretty(&nfpm_config(inputs, packager, &entries, &staged))
        .map_err(|e| e.to_string())?;
    fs::write(&config, text).map_err(|e| format!("{}: {e}", config.display()))?;
    let name = format!("{}.{packager}", stem(&inputs.version, inputs.arch.name));
    let target = dist.join(&name);
    if target.exists() {
        fs::remove_file(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    let tool = std::env::var("NFPM").unwrap_or_else(|_| "nfpm".into());
    crate::run(
        packager,
        &tool,
        &[
            "package",
            "--config",
            &config.to_string_lossy(),
            "--packager",
            packager,
            "--target",
            &target.to_string_lossy(),
        ],
    )?;
    let bytes = read(&target)?;
    write_with_sums(dist, &name, &bytes)
}

/// Writes `entries` below `dir` with their modes.
fn write_tree(dir: &Path, entries: &[Entry]) -> Result<(), String> {
    for entry in entries {
        let path = dir.join(&entry.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&path, &entry.content).map_err(|e| format!("{}: {e}", path.display()))?;
        set_mode(&path, entry.mode)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<(), String> {
    Ok(())
}

/// A gzip-compressed tar archive of `entries`, with their folders, owned by root and with a fixed
/// time, so the same files give the same archive.
fn tar_gz(entries: &[Entry]) -> Result<Vec<u8>, String> {
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::best()));
    let mut dirs: Vec<String> = Vec::new();
    for entry in entries {
        for (end, _) in entry.path.match_indices('/') {
            let dir = &entry.path[..=end];
            if !dirs.iter().any(|d| d == dir) {
                dirs.push(dir.to_owned());
                append(&mut tar, dir, &[], 0o755, tar::EntryType::Directory)?;
            }
        }
        append(
            &mut tar,
            &entry.path,
            &entry.content,
            entry.mode,
            tar::EntryType::Regular,
        )?;
    }
    let gz = tar.into_inner().map_err(|e| format!("tar: {e}"))?;
    gz.finish().map_err(|e| format!("gzip: {e}"))
}

fn append(
    tar: &mut tar::Builder<GzEncoder<Vec<u8>>>,
    path: &str,
    content: &[u8],
    mode: u32,
    kind: tar::EntryType,
) -> Result<(), String> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_size(content.len() as u64);
    header.set_mode(mode);
    header.set_mtime(ENTRY_TIME);
    header.set_uid(0);
    header.set_gid(0);
    tar.append_data(&mut header, path, content)
        .map_err(|e| format!("tar {path}: {e}"))
}

/// The PNG images of an ICO file as `(size, PNG)`, smallest first; only square PNG entries are
/// taken, and their size is read from the PNG header.
pub(crate) fn ico_pngs(ico: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, String> {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    let u16_at = |at: usize| {
        ico.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |at: usize| {
        ico.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if u16_at(0) != Some(0) || u16_at(2) != Some(1) {
        return Err("the icon is not an ICO file".into());
    }
    let count = usize::from(u16_at(4).unwrap_or(0));
    let mut icons: Vec<(u32, Vec<u8>)> = Vec::new();
    for i in 0..count {
        let at = 6 + 16 * i;
        let (Some(size), Some(offset)) = (u32_at(at + 8), u32_at(at + 12)) else {
            return Err("the ICO directory is cut short".into());
        };
        let (offset, size) = (offset as usize, size as usize);
        let Some(image) = ico.get(offset..offset.saturating_add(size)) else {
            return Err(format!("ICO image {i} lies outside the file"));
        };
        if !image.starts_with(PNG) || image.get(12..16) != Some(b"IHDR") {
            continue;
        }
        let dim = |at: usize| {
            image
                .get(at..at + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        if let (Some(w), Some(h)) = (dim(16), dim(20))
            && w == h
            && !icons.iter().any(|(s, _)| *s == w)
        {
            icons.push((w, image.to_vec()));
        }
    }
    if icons.is_empty() {
        return Err("the ICO file has no square PNG images".into());
    }
    icons.sort_by_key(|(s, _)| *s);
    Ok(icons)
}

/// `bytes` with CRLF line ends turned into LF (a checkout on Windows may have CRLF).
fn lf(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            continue;
        }
        out.push(*b);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn png(size: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(size.to_be_bytes());
        png.extend(size.to_be_bytes());
        png.extend([8, 6, 0, 0, 0]);
        png
    }

    fn ico(images: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0, 0, 1, 0];
        out.extend(u16::try_from(images.len()).unwrap().to_le_bytes());
        let mut offset = 6 + 16 * images.len();
        let mut data: Vec<u8> = Vec::new();
        for image in images {
            out.extend([0, 0, 0, 0, 1, 0, 32, 0]);
            out.extend(u32::try_from(image.len()).unwrap().to_le_bytes());
            out.extend(u32::try_from(offset).unwrap().to_le_bytes());
            offset += image.len();
            data.extend(image);
        }
        out.extend(data);
        out
    }

    fn inputs() -> Inputs {
        Inputs {
            version: "1.2.3".into(),
            arch: linux_arch("x86_64-unknown-linux-gnu").unwrap(),
            binaries: vec![
                ("vibelauncher", b"gui".to_vec()),
                ("vl-cli", b"cli".to_vec()),
            ],
            icons: vec![(16, png(16)), (256, png(256))],
            desktop: b"[Desktop Entry]\n".to_vec(),
            metainfo: b"<component/>\n".to_vec(),
            app_run: b"#!/bin/sh\n".to_vec(),
            license: b"MIT\n".to_vec(),
        }
    }

    fn paths(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn names_follow_the_target_and_the_version() {
        assert_eq!(formats("linux").unwrap().len(), 4);
        assert_eq!(formats("deb"), Some(vec![Format::Deb]));
        assert_eq!(formats("portable"), None);
        let arm = linux_arch("aarch64-unknown-linux-gnu").unwrap();
        assert_eq!((arm.name, arm.deb), ("aarch64", "arm64"));
        assert!(linux_arch("x86_64-pc-windows-msvc").is_err());
        assert!(linux_arch("x86_64-unknown-linux-musl").is_err());
        assert_eq!(stem("1.2.3", "x86_64"), "vibelauncher-1.2.3-linux-x86_64");
        assert_eq!(
            update_information("x86_64"),
            "gh-releases-zsync|FQ401|vibelauncher|latest|vibelauncher-*-linux-x86_64.AppImage.zsync"
        );
    }

    #[test]
    fn the_icons_are_the_png_images_of_the_ico_file() {
        let icons = ico_pngs(&ico(&[png(48), b"BMP".to_vec(), png(16), png(48)])).unwrap();
        let sizes: Vec<u32> = icons.iter().map(|(s, _)| *s).collect();
        assert_eq!(sizes, [16, 48]);
        assert_eq!(icons[0].1, png(16));
        assert!(ico_pngs(&ico(&[b"BMP".to_vec()])).is_err());
        assert!(ico_pngs(b"not an icon").is_err());
        let mut cut = ico(&[png(16)]);
        cut.truncate(30);
        assert!(ico_pngs(&cut).is_err());
    }

    #[test]
    fn the_placeholder_icon_has_the_hicolor_sizes() {
        let icons =
            ico_pngs(&read(&root().join("assets/icons/vibelauncher.ico")).unwrap()).unwrap();
        let sizes: Vec<u32> = icons.iter().map(|(s, _)| *s).collect();
        assert_eq!(sizes, [16, 24, 32, 48, 64, 256]);
    }

    #[test]
    fn the_tar_archive_is_a_portable_folder_and_reproducible() {
        let entries = tar_entries(&inputs());
        let bytes = tar_gz(&entries).unwrap();
        assert_eq!(bytes, tar_gz(&entries).unwrap());
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
        let mut seen = Vec::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mode = entry.header().mode().unwrap();
            assert_eq!(entry.header().mtime().unwrap(), ENTRY_TIME);
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            seen.push((path, mode, content));
        }
        let dir = "vibelauncher-1.2.3-linux-x86_64";
        let names: Vec<&str> = seen.iter().map(|(p, _, _)| p.as_str()).collect();
        assert_eq!(
            names,
            [
                format!("{dir}/"),
                format!("{dir}/vibelauncher"),
                format!("{dir}/vl-cli"),
                format!("{dir}/{PORTABLE_MARKER}"),
                format!("{dir}/LICENSE.txt"),
            ]
        );
        assert_eq!(seen[1].1, 0o755);
        assert_eq!(seen[1].2, b"gui");
        assert_eq!(seen[3].1, 0o644);
    }

    #[test]
    fn the_app_dir_runs_app_run_and_names_the_metadata_as_appimagetool_reads_it() {
        let entries = app_dir_entries(&inputs()).unwrap();
        let names = paths(&entries);
        assert_eq!(
            names[..3],
            [
                "AppRun",
                "me.vibefork.VibeLauncher.desktop",
                "me.vibefork.VibeLauncher.png"
            ]
        );
        assert_eq!(entries[0].mode, 0o755);
        assert_eq!(entries[2].content, png(256));
        for name in [
            "usr/bin/vibelauncher",
            "usr/bin/vl-cli",
            "usr/share/applications/me.vibefork.VibeLauncher.desktop",
            "usr/share/icons/hicolor/16x16/apps/me.vibefork.VibeLauncher.png",
            "usr/share/icons/hicolor/256x256/apps/me.vibefork.VibeLauncher.png",
            "usr/share/metainfo/me.vibefork.VibeLauncher.appdata.xml",
        ] {
            assert!(names.contains(&name), "{name}");
        }
        // No marker: the AppImage is recognized by the `APPIMAGE` variable.
        assert!(
            !names
                .iter()
                .any(|n| n.contains(INSTALL_MARKER) || n.contains(PORTABLE_MARKER))
        );
    }

    #[test]
    fn system_packages_mark_their_install_method_next_to_the_executables() {
        let config_of = |packager: &str| {
            let entries = system_entries(&inputs(), packager);
            let config = nfpm_config(&inputs(), packager, &entries, Path::new("/stage"));
            (entries, config)
        };
        let (deb, config) = config_of("deb");
        let marker = deb
            .iter()
            .find(|e| e.path == "usr/lib/vibelauncher/vibelauncher.install")
            .unwrap();
        assert_eq!(marker.content, b"deb\n");
        assert!(paths(&deb).contains(&"usr/share/doc/vibelauncher/copyright"));
        assert!(paths(&deb).contains(&"usr/share/metainfo/me.vibefork.VibeLauncher.metainfo.xml"));
        assert_eq!(config["arch"], "amd64");
        assert_eq!(config["version"], "1.2.3");
        let contents = config["contents"].as_array().unwrap();
        let link = contents
            .iter()
            .find(|c| c["dst"] == "/usr/bin/vl-cli")
            .unwrap();
        assert_eq!(link["type"], "symlink");
        assert_eq!(link["src"], "/usr/lib/vibelauncher/vl-cli");
        let exe = contents
            .iter()
            .find(|c| c["dst"] == "/usr/lib/vibelauncher/vibelauncher")
            .unwrap();
        assert_eq!(exe["file_info"]["mode"], 0o755);
        assert!(
            contents
                .iter()
                .any(|c| c["dst"] == LIB_DIR && c["type"] == "dir")
        );

        let (rpm, _) = config_of("rpm");
        let marker = rpm
            .iter()
            .find(|e| e.path.ends_with(INSTALL_MARKER))
            .unwrap();
        assert_eq!(marker.content, b"rpm\n");
        assert!(paths(&rpm).contains(&"usr/share/licenses/vibelauncher/LICENSE"));
    }

    #[test]
    fn the_desktop_files_name_the_application_id() {
        let linux = root().join("packaging").join("linux");
        let desktop =
            String::from_utf8(lf(&read(&linux.join(format!("{APP_ID}.desktop"))).unwrap()))
                .unwrap();
        assert!(desktop.contains(&format!("\nIcon={APP_ID}\n")));
        assert!(desktop.contains(&format!("\nStartupWMClass={APP_ID}\n")));
        assert!(desktop.contains("\nExec=vibelauncher\n"));
        let metainfo =
            String::from_utf8(read(&linux.join(format!("{APP_ID}.metainfo.xml"))).unwrap())
                .unwrap();
        assert!(metainfo.contains(&format!("<id>{APP_ID}</id>")));
        assert!(metainfo.contains(&format!(">{APP_ID}.desktop</launchable>")));
        // The desktop app's Wayland app ID and X11 class match the desktop entry.
        let app = read(&root().join("apps/vibelauncher/src/main.rs")).unwrap();
        assert!(
            String::from_utf8(app)
                .unwrap()
                .contains(&format!("\"{APP_ID}\""))
        );
        assert_eq!(lf(b"a\r\nb\rc\n"), b"a\nb\rc\n");
    }
}
