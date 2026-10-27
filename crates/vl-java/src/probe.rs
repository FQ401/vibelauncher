//! Checking one Java executable: run it, parse what it reports, never trust its location.
//!
//! The check is `java -XshowSettings:properties -version`. Every Java from 8 on prints its system
//! properties to standard error, so the answer comes from the runtime itself and not from a
//! directory name or a `release` file that a repackaged runtime may carry wrongly.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vl_core::java::JavaRuntime;
use vl_core::platform::Arch;

use crate::run::{RunError, run_capped};

/// Longest output kept from one check.
const OUTPUT_CAP: usize = 256 * 1024;

/// Why a candidate is not a usable Java.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// The executable could not be started.
    Spawn(String),
    /// The check did not finish in time.
    Timeout,
    /// The program ran but did not report a Java version.
    NotJava,
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "cannot start: {e}"),
            Self::Timeout => f.write_str("the check timed out"),
            Self::NotJava => f.write_str("not a Java runtime"),
        }
    }
}

impl std::error::Error for ProbeError {}

/// A checked runtime together with the `java.home` it reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    /// The runtime (`managed` is `false`).
    pub runtime: JavaRuntime,
    /// The `java.home` property: the real installation behind a launcher stub or a link.
    pub home: Option<PathBuf>,
}

/// Runs the check on `java` and returns the runtime it describes (`managed` is `false`).
pub fn probe(java: &Path, timeout: Duration) -> Result<JavaRuntime, ProbeError> {
    probe_with_home(java, timeout).map(|p| p.runtime)
}

/// Like [`probe`], and also returns `java.home`, which tells apart a runtime from the stub
/// executables that point to it (such as `Common Files\Oracle\Java\javapath\java.exe`).
pub fn probe_with_home(java: &Path, timeout: Duration) -> Result<Probed, ProbeError> {
    let output = run_capped(
        java,
        &["-XshowSettings:properties", "-version"],
        &["JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS"],
        timeout,
        OUTPUT_CAP,
    )
    .map_err(|e| match e {
        RunError::Spawn(m) => ProbeError::Spawn(m),
        RunError::Timeout => ProbeError::Timeout,
    })?;
    let mut runtime = parse_properties(&output.text).ok_or(ProbeError::NotJava)?;
    runtime.path = java.to_path_buf();
    Ok(Probed {
        home: parse_java_home(&output.text),
        runtime,
    })
}

/// The `java.home` property of a property listing, if present.
pub fn parse_java_home(text: &str) -> Option<PathBuf> {
    text.lines()
        .filter_map(|line| line.strip_prefix("    java.home = "))
        .map(str::trim)
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Reads the output of `java -XshowSettings:properties -version`.
///
/// Property lines look like `    java.version = 17.0.2`; continuation lines of multi-valued
/// properties and the `-version` banner are ignored. Returns `None` without a usable
/// `java.version`. The path of the result is empty.
pub fn parse_properties(text: &str) -> Option<JavaRuntime> {
    let mut props: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        // Property lines are indented by exactly four spaces; continuations by eight.
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue;
        }
        if let Some((key, value)) = rest.split_once(" = ")
            && !key.is_empty()
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        {
            props.entry(key).or_insert_with(|| value.trim());
        }
    }
    let version = props
        .get("java.version")
        .copied()
        .filter(|v| !v.is_empty())?;
    let major = parse_major(version)?;
    Some(JavaRuntime {
        path: PathBuf::new(),
        major,
        version: version.to_owned(),
        arch: arch_from_java(
            props.get("os.arch").copied().unwrap_or_default(),
            props.get("sun.arch.data.model").copied(),
        ),
        vendor: props
            .get("java.vendor")
            .copied()
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown")
            .to_owned(),
        managed: false,
    })
}

/// Reads the `release` file of a Java home (`KEY="value"` lines). Meant for tooling that must
/// not start a runtime; the result is a hint, not a check.
pub fn parse_release(text: &str) -> Option<JavaRuntime> {
    let mut props: HashMap<&str, &str> = HashMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.trim().split_once('=') {
            props.insert(key.trim(), value.trim().trim_matches('"'));
        }
    }
    let version = props
        .get("JAVA_VERSION")
        .copied()
        .filter(|v| !v.is_empty())?;
    Some(JavaRuntime {
        path: PathBuf::new(),
        major: parse_major(version)?,
        version: version.to_owned(),
        arch: arch_from_java(props.get("OS_ARCH").copied().unwrap_or_default(), None),
        vendor: props
            .get("IMPLEMENTOR")
            .copied()
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown")
            .to_owned(),
        managed: false,
    })
}

/// The major version of a `java.version` string: `1.8.0_345` is 8, `17.0.2` is 17, `21-ea` is 21.
pub fn parse_major(version: &str) -> Option<u32> {
    let mut parts = version
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty());
    let first: u32 = parts.next()?.parse().ok()?;
    let major = if first == 1 {
        parts.next()?.parse().ok()?
    } else {
        first
    };
    (major >= 1).then_some(major)
}

/// Maps `os.arch` (and `sun.arch.data.model` when `os.arch` is unclear) to an [`Arch`].
pub fn arch_from_java(os_arch: &str, data_model: Option<&str>) -> Arch {
    match os_arch.trim().to_ascii_lowercase().as_str() {
        "amd64" | "x86_64" | "x64" => Arch::X86_64,
        "aarch64" | "arm64" => Arch::Aarch64,
        "x86" | "i386" | "i486" | "i586" | "i686" => Arch::X86,
        s if s.starts_with("armv") || s == "arm" => Arch::Arm,
        _ => match data_model {
            Some("32") => Arch::X86,
            _ => Arch::Other,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODERN: &str = "Property settings:
    file.encoding = UTF-8
    java.class.path =
    java.home = /opt/synthetic/jdk-17
    java.vendor = Synthetic Vendor
    java.version = 17.0.2
    java.vm.name = Synthetic 64-Bit Server VM
    os.arch = aarch64
    sun.boot.library.path = /opt/a
        /opt/b
    sun.arch.data.model = 64

openjdk version \"17.0.2\" 2022-01-18
";

    #[test]
    fn parses_the_property_listing() {
        let r = parse_properties(MODERN).unwrap();
        assert_eq!(r.major, 17);
        assert_eq!(r.version, "17.0.2");
        assert_eq!(r.arch, Arch::Aarch64);
        assert_eq!(r.vendor, "Synthetic Vendor");
        assert!(!r.managed);
    }

    #[test]
    fn reads_java_home() {
        assert_eq!(
            parse_java_home(MODERN),
            Some(PathBuf::from("/opt/synthetic/jdk-17"))
        );
        assert_eq!(
            parse_java_home(
                "    java.version = 8
"
            ),
            None
        );
        assert_eq!(
            parse_java_home(
                "        java.home = nested
"
            ),
            None
        );
    }

    #[test]
    fn parses_old_versions_and_32_bit() {
        let text =
            "    java.version = 1.8.0_345\n    java.vendor = Old Vendor\n    os.arch = x86\n";
        let r = parse_properties(text).unwrap();
        assert_eq!((r.major, r.arch), (8, Arch::X86));
    }

    #[test]
    fn rejects_output_without_a_version() {
        assert!(parse_properties("").is_none());
        assert!(parse_properties("Error: Could not create the Java Virtual Machine.").is_none());
        assert!(parse_properties("    java.version = \n").is_none());
        assert!(parse_properties("    java.version = banana\n").is_none());
    }

    #[test]
    fn continuation_lines_do_not_override_properties() {
        let text =
            "    java.library.path = /a\n        java.version = 99\n    java.version = 21.0.1\n";
        assert_eq!(parse_properties(text).unwrap().major, 21);
    }

    #[test]
    fn reads_major_versions() {
        for (v, m) in [
            ("1.7.0_80", 7),
            ("1.8.0_345", 8),
            ("9", 9),
            ("9.0.4", 9),
            ("11.0.21", 11),
            ("21-ea", 21),
            ("25", 25),
        ] {
            assert_eq!(parse_major(v), Some(m), "{v}");
        }
        for v in ["", "x", "1", "0", "1.0"] {
            assert_eq!(parse_major(v), None, "{v}");
        }
    }

    #[test]
    fn maps_architectures() {
        assert_eq!(arch_from_java("amd64", None), Arch::X86_64);
        assert_eq!(arch_from_java("x86_64", None), Arch::X86_64);
        assert_eq!(arch_from_java("arm64", None), Arch::Aarch64);
        assert_eq!(arch_from_java("i386", None), Arch::X86);
        assert_eq!(arch_from_java("armv7l", None), Arch::Arm);
        assert_eq!(arch_from_java("riscv64", None), Arch::Other);
        assert_eq!(arch_from_java("", Some("32")), Arch::X86);
    }

    #[test]
    fn parses_a_release_file() {
        let text = "IMPLEMENTOR=\"Synthetic\"\nJAVA_VERSION=\"21.0.3\"\nOS_ARCH=\"x86_64\"\nOS_NAME=\"Linux\"\n";
        let r = parse_release(text).unwrap();
        assert_eq!(
            (r.major, r.arch, r.vendor.as_str()),
            (21, Arch::X86_64, "Synthetic")
        );
        assert!(parse_release("OS_ARCH=\"x86_64\"").is_none());
    }

    #[test]
    fn a_missing_executable_is_an_error_not_a_panic() {
        let err = probe(
            Path::new("/definitely/not/here/java"),
            Duration::from_secs(1),
        );
        assert!(matches!(err, Err(ProbeError::Spawn(_))));
    }
}
