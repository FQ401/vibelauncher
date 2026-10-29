//! macOS: installing an update from a disk image over the installed `.app` bundle.
//!
//! The image is attached read-only at a private mount point (`hdiutil attach -nobrowse`), its
//! one top-level `.app` bundle is copied next to the installed bundle (`ditto`, without
//! quarantine information) and its signature checked (`codesign --verify --deep --strict`);
//! the image is detached. Then the installed bundle is renamed away, the copy renamed into its
//! place and the old bundle removed; a failure puts the old bundle back.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::UpdaterError;

/// Suffix of the copy of the new bundle next to the installed one (`.<name>.vl-update-new`).
pub const BUNDLE_NEW_SUFFIX: &str = ".vl-update-new";
/// Suffix of the installed bundle while it is replaced (`.<name>.vl-update-old`).
pub const BUNDLE_OLD_SUFFIX: &str = ".vl-update-old";

/// `.<name><suffix>` next to `bundle`.
fn sibling(bundle: &Path, suffix: &str) -> Result<PathBuf, UpdaterError> {
    let name = bundle
        .file_name()
        .ok_or_else(|| UpdaterError::Io(format!("{} is not a bundle", bundle.display())))?;
    let mut hidden = OsString::from(".");
    hidden.push(name);
    hidden.push(suffix);
    Ok(bundle.with_file_name(hidden))
}

/// Removes what a bundle update left next to `bundle`; best effort, for the first start after an
/// update.
pub fn clean_app_bundle(bundle: &Path) {
    for suffix in [BUNDLE_NEW_SUFFIX, BUNDLE_OLD_SUFFIX] {
        if let Ok(path) = sibling(bundle, suffix) {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// Installs the application bundle of the disk image `image` over `bundle`.
pub fn replace_app_bundle(image: &Path, bundle: &Path) -> Result<(), UpdaterError> {
    if !cfg!(target_os = "macos") {
        return Err(UpdaterError::NotImplemented(
            "installing a disk image outside macOS",
        ));
    }
    let new = sibling(bundle, BUNDLE_NEW_SUFFIX)?;
    let old = sibling(bundle, BUNDLE_OLD_SUFFIX)?;
    remove_if_exists(&new)?;
    remove_if_exists(&old)?;
    let copied = copy_out(image, &new);
    if let Err(e) = copied {
        let _ = fs::remove_dir_all(&new);
        return Err(e);
    }
    let had_old = fs::symlink_metadata(bundle).is_ok();
    if had_old && let Err(e) = fs::rename(bundle, &old) {
        let _ = fs::remove_dir_all(&new);
        return Err(UpdaterError::io(bundle, &e));
    }
    if let Err(e) = fs::rename(&new, bundle) {
        if had_old {
            let _ = fs::rename(&old, bundle);
        }
        let _ = fs::remove_dir_all(&new);
        return Err(UpdaterError::io(bundle, &e));
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// Attaches `image`, copies its bundle to `to` and checks the copy's signature; the image is
/// always detached again.
fn copy_out(image: &Path, to: &Path) -> Result<(), UpdaterError> {
    let mount = mount_point(image)?;
    remove_if_exists(&mount)?;
    fs::create_dir_all(&mount).map_err(|e| UpdaterError::io(&mount, &e))?;
    let attached = tool(
        "hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-noautoopen".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            image.as_os_str(),
        ],
    );
    let result = attached.and_then(|()| {
        let source = find_bundle(&mount)?;
        tool(
            "ditto",
            &["--noqtn".as_ref(), source.as_os_str(), to.as_os_str()],
        )?;
        tool(
            "codesign",
            &[
                "--verify".as_ref(),
                "--deep".as_ref(),
                "--strict".as_ref(),
                to.as_os_str(),
            ],
        )
        .map_err(|e| UpdaterError::Integrity(format!("the new bundle's signature: {e}")))
    });
    detach(&mount);
    let _ = fs::remove_dir(&mount);
    result
}

/// The private mount point of `image`: a folder next to it.
fn mount_point(image: &Path) -> Result<PathBuf, UpdaterError> {
    let dir = image
        .parent()
        .ok_or_else(|| UpdaterError::Io(format!("{} has no folder", image.display())))?;
    Ok(dir.join("mount"))
}

/// The one `.app` folder at the top of the mounted image (not a link).
fn find_bundle(mount: &Path) -> Result<PathBuf, UpdaterError> {
    let entries = fs::read_dir(mount).map_err(|e| UpdaterError::io(mount, &e))?;
    let mut bundles = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| UpdaterError::io(mount, &e))?;
        let is_app = Path::new(&entry.file_name())
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"));
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_app && is_dir {
            bundles.push(entry.path());
        }
    }
    match bundles.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(UpdaterError::Integrity(
            "the disk image holds no application bundle".into(),
        )),
        _ => Err(UpdaterError::Integrity(
            "the disk image holds more than one application bundle".into(),
        )),
    }
}

/// Detaches the image mounted at `mount`, forced when it is busy.
fn detach(mount: &Path) {
    let plain = tool("hdiutil", &["detach".as_ref(), mount.as_os_str()]);
    if plain.is_err() {
        let _ = tool(
            "hdiutil",
            &["detach".as_ref(), "-force".as_ref(), mount.as_os_str()],
        );
    }
}

fn remove_if_exists(path: &Path) -> Result<(), UpdaterError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(UpdaterError::io(path, &e)),
    }
}

/// Runs a system tool; a failure carries what it printed to stderr.
fn tool(program: &str, args: &[&std::ffi::OsStr]) -> Result<(), UpdaterError> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| UpdaterError::Io(format!("cannot run {program}: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(UpdaterError::Io(format!(
            "{program} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Dir;

    #[test]
    fn leftovers_sit_next_to_the_bundle_and_are_cleaned() {
        let dir = Dir::new();
        let bundle = dir.path().join("VibeLauncher.app");
        let new = sibling(&bundle, BUNDLE_NEW_SUFFIX).unwrap();
        let old = sibling(&bundle, BUNDLE_OLD_SUFFIX).unwrap();
        assert_eq!(new, dir.path().join(".VibeLauncher.app.vl-update-new"));
        assert_eq!(old, dir.path().join(".VibeLauncher.app.vl-update-old"));
        fs::create_dir_all(new.join("Contents")).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        clean_app_bundle(&bundle);
        assert!(!new.exists() && !old.exists());
        assert!(bundle.exists());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_systems_do_not_install_disk_images() {
        let dir = Dir::new();
        assert!(matches!(
            replace_app_bundle(&dir.path().join("x.dmg"), &dir.path().join("X.app")),
            Err(UpdaterError::NotImplemented(_))
        ));
    }
}

/// Tests with real disk images; they need `hdiutil`, `ditto` and `codesign`.
#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use super::*;
    use crate::testkit::Dir;

    /// A bundle with `text` in `Contents/Resources/version.txt`, signed ad hoc. Its executable
    /// is a copy of a system Mach-O file (it is never started), so the signature lives in the
    /// file itself.
    fn make_bundle(app: &Path, text: &str) {
        let macos = app.join("Contents/MacOS");
        let resources = app.join("Contents/Resources");
        fs::create_dir_all(&macos).unwrap();
        fs::create_dir_all(&resources).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<plist version=\"1.0\"><dict>             <key>CFBundleIdentifier</key><string>me.vibefork.VibeLauncher.test</string>             <key>CFBundleExecutable</key><string>vibelauncher</string>             <key>CFBundlePackageType</key><string>APPL</string></dict></plist>
",
        )
        .unwrap();
        fs::write(resources.join("version.txt"), text).unwrap();
        let exe = macos.join("vibelauncher");
        fs::copy("/usr/bin/true", &exe).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        run(
            "codesign",
            &["--force", "--sign", "-", app.to_str().unwrap()],
        );
    }

    /// A compressed disk image of `folder`.
    fn make_image(folder: &Path, image: &Path) {
        let args = [
            "create",
            "-volname",
            "VibeLauncher test",
            "-srcfolder",
            folder.to_str().unwrap(),
            "-fs",
            "HFS+",
            "-format",
            "UDZO",
            "-ov",
            image.to_str().unwrap(),
        ];
        // `hdiutil create` now and then fails with "Resource busy".
        for _ in 0..3 {
            if Command::new("hdiutil")
                .args(args)
                .status()
                .unwrap()
                .success()
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        panic!("hdiutil create failed");
    }

    fn run(program: &str, args: &[&str]) {
        assert!(Command::new(program).args(args).status().unwrap().success());
    }

    /// The `version.txt` of `bundle`.
    fn text_of(bundle: &Path) -> String {
        fs::read_to_string(bundle.join("Contents/Resources/version.txt")).unwrap()
    }

    fn mounted(mount: &Path) -> bool {
        let out = Command::new("mount").output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains(mount.to_str().unwrap())
    }

    /// An installed bundle of text `old` and a staged image; returns (bundle, image).
    fn setup(dir: &Path, image_content: impl FnOnce(&Path)) -> (PathBuf, PathBuf) {
        let apps = dir.join("Applications");
        let bundle = apps.join("VibeLauncher.app");
        make_bundle(&bundle, "old");
        let source = dir.join("source");
        fs::create_dir_all(&source).unwrap();
        image_content(&source);
        let staging = dir.join("staging");
        fs::create_dir_all(&staging).unwrap();
        let image = staging.join("update.dmg");
        make_image(&source, &image);
        (bundle, image)
    }

    fn exe(bundle: &Path) -> PathBuf {
        bundle.join("Contents/MacOS/vibelauncher")
    }

    fn assert_clean(bundle: &Path, image: &Path) {
        let mount = mount_point(image).unwrap();
        assert!(!mounted(&mount), "the image is still attached");
        assert!(!mount.exists());
        assert!(!sibling(bundle, BUNDLE_NEW_SUFFIX).unwrap().exists());
        assert!(!sibling(bundle, BUNDLE_OLD_SUFFIX).unwrap().exists());
    }

    #[test]
    fn the_bundle_of_the_image_replaces_the_installed_one() {
        let dir = Dir::new();
        let (bundle, image) = setup(dir.path(), |source| {
            make_bundle(&source.join("VibeLauncher.app"), "new");
            std::os::unix::fs::symlink("/Applications", source.join("Applications")).unwrap();
            fs::write(source.join("note.txt"), b"note").unwrap();
        });
        assert_eq!(text_of(&bundle), "old");
        replace_app_bundle(&image, &bundle).unwrap();
        assert_eq!(text_of(&bundle), "new");
        assert!(!bundle.join("note.txt").exists());
        assert_clean(&bundle, &image);
        let xattrs = Command::new("xattr").arg(&bundle).output().unwrap();
        assert!(!String::from_utf8_lossy(&xattrs.stdout).contains("com.apple.quarantine"));
    }

    #[test]
    fn a_missing_bundle_is_installed_too() {
        let dir = Dir::new();
        let (bundle, image) = setup(dir.path(), |source| {
            make_bundle(&source.join("VibeLauncher.app"), "new");
        });
        fs::remove_dir_all(&bundle).unwrap();
        replace_app_bundle(&image, &bundle).unwrap();
        assert_eq!(text_of(&bundle), "new");
        assert_clean(&bundle, &image);
    }

    #[test]
    fn an_image_without_one_bundle_changes_nothing() {
        for two in [false, true] {
            let dir = Dir::new();
            let (bundle, image) = setup(dir.path(), |source| {
                fs::write(source.join("note.txt"), b"note").unwrap();
                if two {
                    make_bundle(&source.join("A.app"), "a");
                    make_bundle(&source.join("B.app"), "b");
                }
            });
            assert!(matches!(
                replace_app_bundle(&image, &bundle),
                Err(UpdaterError::Integrity(_))
            ));
            assert_eq!(text_of(&bundle), "old");
            assert_clean(&bundle, &image);
        }
    }

    #[test]
    fn a_bundle_changed_after_signing_changes_nothing() {
        let dir = Dir::new();
        let (bundle, image) = setup(dir.path(), |source| {
            let app = source.join("VibeLauncher.app");
            make_bundle(&app, "new");
            fs::write(app.join("Contents/Resources/version.txt"), "changed").unwrap();
        });
        assert!(matches!(
            replace_app_bundle(&image, &bundle),
            Err(UpdaterError::Integrity(_))
        ));
        assert_eq!(text_of(&bundle), "old");
        assert_clean(&bundle, &image);
    }

    #[test]
    fn a_damaged_image_changes_nothing() {
        let dir = Dir::new();
        let (bundle, image) = setup(dir.path(), |source| {
            make_bundle(&source.join("VibeLauncher.app"), "new");
        });
        fs::write(&image, b"not a disk image").unwrap();
        assert!(replace_app_bundle(&image, &bundle).is_err());
        assert_eq!(text_of(&bundle), "old");
        assert_clean(&bundle, &image);
    }

    /// The release disk image named by `VL_TEST_DMG` (the `package` workflow sets it) replaces
    /// an installed bundle, and the new launcher starts.
    #[test]
    fn the_release_image_installs() {
        let Some(release) = std::env::var_os("VL_TEST_DMG") else {
            return;
        };
        let dir = Dir::new();
        let apps = dir.path().join("Applications");
        let bundle = apps.join("VibeLauncher.app");
        make_bundle(&bundle, "old");
        let staging = dir.path().join("staging");
        fs::create_dir_all(&staging).unwrap();
        let image = staging.join("update.dmg");
        fs::copy(release, &image).unwrap();
        replace_app_bundle(&image, &bundle).unwrap();
        let out = Command::new(exe(&bundle))
            .arg("--version")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("vibelauncher "));
        assert!(bundle.join("Contents/MacOS/vl-cli").is_file());
        assert_clean(&bundle, &image);
    }
}
