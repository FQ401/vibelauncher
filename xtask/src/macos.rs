//! `cargo xtask package macos`: the universal macOS disk image.
//!
//! Builds `vibelauncher` and `vl-cli` in release mode (symbols stripped) for
//! `aarch64-apple-darwin` and `x86_64-apple-darwin` with `MACOSX_DEPLOYMENT_TARGET` set to
//! [`MIN_MACOS`], joins each pair into a universal binary (`lipo`) and puts both into
//! `VibeLauncher.app` with its `Info.plist` and icon. The bundle is signed ad hoc
//! (`codesign --sign -`): no Developer ID, no notarization. Writes
//! `target/dist/vibelauncher-<version>-macos-universal.dmg` (`hdiutil`, compressed, HFS+) with a
//! `.sha256` file next to it; the image holds the bundle, a link to `/Applications` and a short
//! note on opening an app that is not notarized.
//!
//! Runs on macOS only: `lipo`, `codesign` and `hdiutil` are part of the system.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::package::{BINARIES, build_release_with, read, write_with_sums};
use crate::{read_version, root};

/// The bundle identifier (the application ID of every package).
const BUNDLE_ID: &str = crate::linux::APP_ID;
/// The bundle's folder name; the self-update looks for it in the disk image.
pub const BUNDLE: &str = "VibeLauncher.app";
/// The bundle's main executable.
const EXECUTABLE: &str = "vibelauncher";
/// The icon file in `Contents/Resources`.
const ICON: &str = "VibeLauncher.icns";
/// The oldest macOS the executables run on: `MACOSX_DEPLOYMENT_TARGET` and
/// `LSMinimumSystemVersion`. 11 (Big Sur) is the first release for Apple silicon and the oldest
/// the `aarch64-apple-darwin` target supports, so both halves of the universal binary share it.
pub const MIN_MACOS: &str = "11.0";
/// The two targets of the universal binary.
const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
/// The name of the note in the disk image.
const NOTE: &str = "If VibeLauncher does not open.txt";

/// Builds the universal disk image.
pub fn package() -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("the macOS disk image is built on macOS (lipo, codesign, hdiutil)".into());
    }
    let version = read_version()?;
    for target in TARGETS {
        build_release_with(target, true, &[("MACOSX_DEPLOYMENT_TARGET", MIN_MACOS)])?;
    }

    let dist = root().join("target").join("dist");
    let staging = dist.join("staging-macos");
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    let image_root = staging.join("image");
    let app = image_root.join(BUNDLE);
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    for dir in [&macos, &resources] {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }

    for (_, bin) in BINARIES {
        let slices: Vec<String> = TARGETS
            .iter()
            .map(|t| {
                let path = root().join("target").join(t).join("release").join(bin);
                path.to_string_lossy().into_owned()
            })
            .collect();
        let out = macos.join(bin).to_string_lossy().into_owned();
        let mut args = vec!["-create", "-output", &out];
        args.extend(slices.iter().map(String::as_str));
        tool("lipo", &args)?;
        let archs = output("lipo", &["-archs", &out])?;
        if !has_both_archs(&archs) {
            return Err(format!("lipo: {out} holds `{archs}`, not x86_64 and arm64"));
        }
    }
    write(
        &contents.join("Info.plist"),
        info_plist(&version).as_bytes(),
    )?;
    write(&contents.join("PkgInfo"), b"APPL????")?;
    let icons = crate::linux::ico_pngs(&read(&root().join("assets/icons/vibelauncher.ico"))?)?;
    write(&resources.join(ICON), &icns(&icons)?)?;
    write(
        &resources.join("LICENSE.txt"),
        &read(&root().join("LICENSE"))?,
    )?;

    sign(&app)?;

    link_applications(&image_root.join("Applications"))?;
    write(&image_root.join(NOTE), NOTE_TEXT.as_bytes())?;

    let name = image_name(&version);
    let image = staging.join(&name);
    let volume = format!("VibeLauncher {version}");
    let image_root_arg = image_root.to_string_lossy();
    let image_arg = image.to_string_lossy();
    let create = [
        "create",
        "-volname",
        &volume,
        "-srcfolder",
        &image_root_arg,
        "-fs",
        "HFS+",
        "-format",
        "UDZO",
        "-imagekey",
        "zlib-level=9",
        "-ov",
        &image_arg,
    ];
    // `hdiutil create` now and then fails with "Resource busy" on a busy machine.
    let mut tries = 0;
    loop {
        tries += 1;
        match tool("hdiutil", &create) {
            Ok(()) => break,
            Err(e) if tries < 3 => {
                eprintln!("{e}; trying again");
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            Err(e) => return Err(e),
        }
    }
    tool("hdiutil", &["verify", &image_arg])?;
    write_with_sums(&dist, &name, &read(&image)?)
}

/// Signs the bundle ad hoc: first the second executable, then the bundle (its main executable
/// and the seal over its resources); then checks the signature strictly.
fn sign(app: &Path) -> Result<(), String> {
    let app_arg = app.to_string_lossy();
    // Extended attributes (Finder information, provenance) break the signature's seal.
    tool("xattr", &["-cr", &app_arg])?;
    let cli = app.join("Contents/MacOS/vl-cli");
    let cli_id = format!("{BUNDLE_ID}.vl-cli");
    let cli_arg = cli.to_string_lossy();
    tool(
        "codesign",
        &[
            "--force",
            "--sign",
            "-",
            "--timestamp=none",
            "--identifier",
            &cli_id,
            &cli_arg,
        ],
    )?;
    tool(
        "codesign",
        &["--force", "--sign", "-", "--timestamp=none", &app_arg],
    )?;
    tool(
        "codesign",
        &["--verify", "--deep", "--strict", "--verbose=2", &app_arg],
    )
}

/// `vibelauncher-<version>-macos-universal.dmg`.
fn image_name(version: &str) -> String {
    format!("vibelauncher-{version}-macos-universal.dmg")
}

/// Whether `lipo -archs` printed both architectures.
fn has_both_archs(archs: &str) -> bool {
    let archs: Vec<&str> = archs.split_whitespace().collect();
    archs.contains(&"x86_64") && archs.contains(&"arm64")
}

/// The bundle version: the numeric `X.Y.Z` of `version` (a pre-release suffix dropped), as
/// `CFBundleShortVersionString` and `CFBundleVersion` take it.
fn bundle_version(version: &str) -> &str {
    version.split_once('-').map_or(version, |(core, _)| core)
}

/// The bundle's `Info.plist`.
fn info_plist(version: &str) -> String {
    let short = bundle_version(version);
    let entries: [(&str, Value); 17] = [
        ("CFBundleDevelopmentRegion", Value::Str("en")),
        ("CFBundleDisplayName", Value::Str("VibeLauncher")),
        ("CFBundleExecutable", Value::Str(EXECUTABLE)),
        ("CFBundleIconFile", Value::Str(ICON)),
        ("CFBundleIdentifier", Value::Str(BUNDLE_ID)),
        ("CFBundleInfoDictionaryVersion", Value::Str("6.0")),
        ("CFBundleName", Value::Str("VibeLauncher")),
        ("CFBundlePackageType", Value::Str("APPL")),
        ("CFBundleShortVersionString", Value::Str(short)),
        ("CFBundleSupportedPlatforms", Value::Array(&["MacOSX"])),
        ("CFBundleVersion", Value::Str(short)),
        (
            "LSApplicationCategoryType",
            Value::Str("public.app-category.games"),
        ),
        ("LSMinimumSystemVersion", Value::Str(MIN_MACOS)),
        ("NSHighResolutionCapable", Value::Bool(true)),
        (
            "NSHumanReadableCopyright",
            Value::Str("Copyright (c) 2026 VibeLauncher contributors"),
        ),
        ("NSPrincipalClass", Value::Str("NSApplication")),
        ("NSSupportsAutomaticGraphicsSwitching", Value::Bool(true)),
    ];
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    for (key, value) in entries {
        out.push_str(&format!("\t<key>{}</key>\n", escape(key)));
        match value {
            Value::Str(s) => out.push_str(&format!("\t<string>{}</string>\n", escape(s))),
            Value::Bool(b) => out.push_str(if b { "\t<true/>\n" } else { "\t<false/>\n" }),
            Value::Array(items) => {
                out.push_str("\t<array>\n");
                for item in items {
                    out.push_str(&format!("\t\t<string>{}</string>\n", escape(item)));
                }
                out.push_str("\t</array>\n");
            }
        }
    }
    out.push_str("</dict>\n</plist>\n");
    out
}

/// A value of the `Info.plist`.
enum Value<'a> {
    Str(&'a str),
    Bool(bool),
    Array(&'a [&'a str]),
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The ICNS types of a PNG image of `size` pixels: the image at 1x and at 2x of half its size.
fn icns_types(size: u32) -> &'static [&'static [u8; 4]] {
    match size {
        16 => &[b"icp4"],
        32 => &[b"icp5", b"ic11"],
        64 => &[b"icp6", b"ic12"],
        128 => &[b"ic07"],
        256 => &[b"ic08", b"ic13"],
        512 => &[b"ic09", b"ic14"],
        1024 => &[b"ic10"],
        _ => &[],
    }
}

/// An ICNS file of the PNG images `icons` (`(size, PNG)`); sizes ICNS has no type for are left
/// out.
fn icns(icons: &[(u32, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    for (size, png) in icons {
        for kind in icns_types(*size) {
            let len = u32::try_from(png.len() + 8).map_err(|_| "icon too large")?;
            body.extend_from_slice(*kind);
            body.extend(len.to_be_bytes());
            body.extend(png);
        }
    }
    if body.is_empty() {
        return Err("the icon has no image of an ICNS size".into());
    }
    let len = u32::try_from(body.len() + 8).map_err(|_| "icon too large")?;
    let mut out = b"icns".to_vec();
    out.extend(len.to_be_bytes());
    out.extend(body);
    Ok(out)
}

/// The note in the disk image.
const NOTE_TEXT: &str = "\
VibeLauncher is signed ad hoc and not notarized by Apple, so macOS asks before the first start.

1. Drag VibeLauncher into Applications.
2. Open it once. When macOS says it cannot check the app, choose Done.
3. Open System Settings > Privacy & Security, scroll to Security and choose Open Anyway
   next to the message about VibeLauncher, then confirm.

macOS remembers this choice; later starts and updates of VibeLauncher open without asking.
";

#[cfg(unix)]
fn link_applications(link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink("/Applications", link)
        .map_err(|e| format!("{}: {e}", link.display()))
}

#[cfg(not(unix))]
fn link_applications(_: &Path) -> Result<(), String> {
    Err("the Applications link needs a Unix system".into())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Runs a system tool from the workspace root.
fn tool(program: &str, args: &[&str]) -> Result<(), String> {
    crate::run(program, program, args)
}

/// Runs a system tool and returns what it printed.
fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_versions() {
        assert_eq!(
            image_name("1.2.3"),
            "vibelauncher-1.2.3-macos-universal.dmg"
        );
        assert_eq!(bundle_version("1.2.3"), "1.2.3");
        assert_eq!(bundle_version("1.2.3-beta.1"), "1.2.3");
        assert!(has_both_archs("x86_64 arm64"));
        assert!(!has_both_archs("arm64"));
        assert!(!has_both_archs("x86_64 arm64e"));
    }

    #[test]
    fn the_info_plist_names_the_bundle() {
        let plist = info_plist("1.2.3-rc.1");
        for line in [
            "<key>CFBundleIdentifier</key>\n\t<string>me.vibefork.VibeLauncher</string>",
            "<key>CFBundleExecutable</key>\n\t<string>vibelauncher</string>",
            "<key>CFBundleIconFile</key>\n\t<string>VibeLauncher.icns</string>",
            "<key>CFBundlePackageType</key>\n\t<string>APPL</string>",
            "<key>CFBundleShortVersionString</key>\n\t<string>1.2.3</string>",
            "<key>CFBundleVersion</key>\n\t<string>1.2.3</string>",
            "<key>LSMinimumSystemVersion</key>\n\t<string>11.0</string>",
            "<key>NSHighResolutionCapable</key>\n\t<true/>",
        ] {
            assert!(plist.contains(line), "{line}");
        }
        assert!(plist.starts_with("<?xml"));
        assert!(plist.ends_with("</plist>\n"));
        // No quarantine for the files the launcher downloads.
        assert!(!plist.contains("LSFileQuarantineEnabled"));
        assert_eq!(plist.matches("<key>").count(), 17);
    }

    #[test]
    fn the_icns_file_holds_the_png_images() {
        let icons = vec![
            (16, b"p16".to_vec()),
            (24, b"p24".to_vec()),
            (32, b"p32".to_vec()),
        ];
        let icns = icns(&icons).unwrap();
        let mut expected = b"icns".to_vec();
        expected.extend(41u32.to_be_bytes());
        for (kind, png) in [(b"icp4", b"p16"), (b"icp5", b"p32"), (b"ic11", b"p32")] {
            expected.extend(kind);
            expected.extend(11u32.to_be_bytes());
            expected.extend(png);
        }
        assert_eq!(icns, expected);
        assert!(super::icns(&[(24, b"p".to_vec())]).is_err());
    }

    #[test]
    fn the_placeholder_icon_gives_an_icns_file() {
        let ico = read(&root().join("assets/icons/vibelauncher.ico")).unwrap();
        let icons = crate::linux::ico_pngs(&ico).unwrap();
        let icns = icns(&icons).unwrap();
        assert_eq!(icns.get(..4), Some(&b"icns"[..]));
        assert_eq!(
            u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize,
            icns.len()
        );
        // 256 px is the largest image: ic08 and ic13.
        assert!(icns.windows(4).any(|w| w == b"ic08"));
    }
}
