//! Platform adjustments for libraries Mojang does not build for the machine.
//!
//! Only macOS on Apple silicon needs them:
//!
//! - **LWJGL 3 without arm64 natives** (versions 1.13 up to 1.18): the `org.lwjgl` libraries are
//!   replaced by the same modules at [`LWJGL3_VERSION`] from Maven Central, with their
//!   `natives-macos-arm64` jars. Later versions already list arm64 natives and are untouched.
//! - **LWJGL 2** (versions up to 1.12.2) has no arm64 build and no drop-in replacement. Such a
//!   version is installed for `x86_64` instead: the libraries and natives are the Intel ones and
//!   the game must run on an `x86_64` Java under Rosetta. [`Adjusted::platform`] carries the
//!   changed platform, and Java selection requires its architecture, so a missing Intel Java is
//!   reported as a Java error with the required architecture.

use std::collections::BTreeSet;

use serde_json::json;
use vl_core::platform::{Arch, Os, Platform};
use vl_meta::libraries::Replacement;
use vl_meta::version::LibraryEntry;

/// The LWJGL release the arm64 replacements use.
pub const LWJGL3_VERSION: &str = "3.3.3";
const MAVEN_CENTRAL: &str = "https://repo1.maven.org/maven2/";

/// The outcome of [`adjust`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adjusted {
    /// The platform to plan the libraries for (and to require from Java).
    pub platform: Platform,
    /// Replacements to apply before planning.
    pub replacements: Vec<Replacement>,
    /// Notes for the user.
    pub notes: Vec<String>,
}

fn is_lwjgl2(name: &str) -> bool {
    name.starts_with("org.lwjgl.lwjgl:")
}

fn lwjgl3_module(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("org.lwjgl:")?;
    rest.split(':').next()
}

fn entry(name: String) -> Option<LibraryEntry> {
    serde_json::from_value(json!({ "name": name, "url": MAVEN_CENTRAL })).ok()
}

/// Decides what the platform needs for `libraries`.
///
pub fn adjust(libraries: &[LibraryEntry], platform: Platform) -> Adjusted {
    let mut adjusted = Adjusted {
        platform,
        replacements: Vec::new(),
        notes: Vec::new(),
    };
    if platform.os != Os::MacOs || platform.arch != Arch::Aarch64 {
        return adjusted;
    }
    if libraries.iter().any(|l| is_lwjgl2(&l.name)) {
        adjusted.platform.arch = Arch::X86_64;
        adjusted.notes.push(
            "this version uses LWJGL 2, which has no Apple silicon build; it runs with an x86_64 \
             Java under Rosetta"
                .to_owned(),
        );
        return adjusted;
    }
    let has_arm64 = libraries
        .iter()
        .any(|l| l.name.contains("natives-macos-arm64"));
    let modules: BTreeSet<&str> = libraries
        .iter()
        .filter_map(|l| lwjgl3_module(&l.name))
        .collect();
    if has_arm64 || modules.is_empty() {
        return adjusted;
    }
    let added = modules
        .iter()
        .flat_map(|m| {
            [
                entry(format!("org.lwjgl:{m}:{LWJGL3_VERSION}")),
                entry(format!("org.lwjgl:{m}:{LWJGL3_VERSION}:natives-macos-arm64")),
            ]
        })
        .flatten()
        .collect();
    adjusted.replacements.push(Replacement {
        os: Some(Os::MacOs),
        arch: Arch::Aarch64,
        drop: vec!["org.lwjgl".to_owned()],
        libraries: added,
    });
    adjusted
        .notes
        .push(format!("LWJGL is replaced by {LWJGL3_VERSION} for Apple silicon"));
    adjusted
}

#[cfg(test)]
mod tests {
    use super::*;
    use vl_meta::libraries::apply_replacements;

    fn lib(name: &str) -> LibraryEntry {
        entry(name.to_owned()).expect("entry")
    }

    const MAC_ARM: Platform = Platform {
        os: Os::MacOs,
        arch: Arch::Aarch64,
    };

    #[test]
    fn other_platforms_are_untouched() {
        let libs = [lib("org.lwjgl:lwjgl:3.2.2")];
        for platform in [
            Platform { os: Os::Windows, arch: Arch::X86_64 },
            Platform { os: Os::MacOs, arch: Arch::X86_64 },
            Platform { os: Os::Linux, arch: Arch::Aarch64 },
        ] {
            let a = adjust(&libs, platform);
            assert_eq!(a.platform, platform);
            assert!(a.replacements.is_empty());
        }
    }

    #[test]
    fn lwjgl3_without_arm64_natives_is_replaced_module_by_module() {
        let libs = [
            lib("org.lwjgl:lwjgl:3.2.2"),
            lib("org.lwjgl:lwjgl:3.2.2:natives-macos"),
            lib("org.lwjgl:lwjgl-glfw:3.2.2"),
            lib("com.example:other:1.0"),
        ];
        let a = adjust(&libs, MAC_ARM);
        assert_eq!(a.platform, MAC_ARM);
        let out = apply_replacements(&libs, &a.replacements, MAC_ARM);
        let names: Vec<&str> = out.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names[0], "com.example:other:1.0");
        assert!(names.contains(&"org.lwjgl:lwjgl:3.3.3:natives-macos-arm64"));
        assert!(names.contains(&"org.lwjgl:lwjgl-glfw:3.3.3"));
        assert!(!names.iter().any(|n| n.contains("3.2.2")));
    }

    #[test]
    fn lwjgl3_with_arm64_natives_is_left_alone() {
        let libs = [
            lib("org.lwjgl:lwjgl:3.3.1"),
            lib("org.lwjgl:lwjgl:3.3.1:natives-macos-arm64"),
        ];
        assert!(adjust(&libs, MAC_ARM).replacements.is_empty());
    }

    #[test]
    fn lwjgl2_moves_to_x86_64() {
        let libs = [lib("org.lwjgl.lwjgl:lwjgl:2.9.4-nightly-20150209")];
        let a = adjust(&libs, MAC_ARM);
        assert_eq!(a.platform.arch, Arch::X86_64);
        assert_eq!(a.platform.os, Os::MacOs);
        assert!(!a.notes.is_empty());
    }
}
