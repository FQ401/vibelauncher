//! Choosing the Java runtime for a game version.

use vl_core::java::JavaRuntime;
use vl_core::platform::Arch;
use vl_core::{Error, ErrorKind, Result};

/// The runtime to launch with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaChoice {
    /// The runtime.
    pub runtime: JavaRuntime,
    /// Whether its major version is the one the game version asks for. When it is newer, old
    /// versions usually still run, but the user should know.
    pub exact: bool,
}

fn arch_fits(runtime: &JavaRuntime, wanted: Arch) -> bool {
    wanted == Arch::Other || runtime.arch == wanted
}

fn no_java(required: u32, wanted: Arch) -> Error {
    Error::new(
        ErrorKind::Java,
        format!("no Java {required} or newer for {wanted:?} was found"),
    )
}

/// The best of `runtimes` for a game version that needs Java `required_major` on architecture
/// `arch`: the exact major version first, else the lowest newer one; managed runtimes before
/// others; the first of equals in the given order.
///
/// # Errors
///
/// `Java` when no runtime has the architecture and at least the required version.
pub fn choose(runtimes: &[JavaRuntime], required_major: u32, arch: Arch) -> Result<JavaChoice> {
    runtimes
        .iter()
        .filter(|r| r.major >= required_major && arch_fits(r, arch))
        .min_by_key(|r| (r.major, !r.managed))
        .map(|r| JavaChoice {
            exact: r.major == required_major,
            runtime: r.clone(),
        })
        .ok_or_else(|| no_java(required_major, arch))
}

/// Checks a runtime the user fixed in the settings: it must be new enough and fit the
/// architecture.
///
/// # Errors
///
/// `Java` when it does not.
pub fn check_fixed(runtime: &JavaRuntime, required_major: u32, arch: Arch) -> Result<JavaChoice> {
    if runtime.major < required_major {
        return Err(Error::new(
            ErrorKind::Java,
            format!(
                "the selected Java is version {} but the game needs {required_major} or newer",
                runtime.major
            ),
        ));
    }
    if !arch_fits(runtime, arch) {
        return Err(Error::new(
            ErrorKind::Java,
            format!(
                "the selected Java is {:?} but the game needs {arch:?}",
                runtime.arch
            ),
        ));
    }
    Ok(JavaChoice {
        exact: runtime.major == required_major,
        runtime: runtime.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn java(major: u32, arch: Arch, managed: bool, path: &str) -> JavaRuntime {
        JavaRuntime {
            path: PathBuf::from(path),
            major,
            version: major.to_string(),
            arch,
            vendor: "Test".into(),
            managed,
        }
    }

    #[test]
    fn exact_major_wins_then_the_lowest_newer() {
        let all = [
            java(21, Arch::X86_64, false, "21"),
            java(17, Arch::X86_64, false, "17"),
            java(8, Arch::X86_64, false, "8"),
        ];
        let c = choose(&all, 17, Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("17"));
        assert!(c.exact);
        let c = choose(&all[..2], 8, Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.major, 17);
        assert!(!c.exact);
    }

    #[test]
    fn architecture_must_fit() {
        let all = [
            java(8, Arch::X86, false, "8-32"),
            java(21, Arch::X86_64, false, "21"),
        ];
        let c = choose(&all, 8, Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("21"));
        let err = choose(&all[..1], 8, Arch::X86_64).expect_err("no fit");
        assert_eq!(err.kind, ErrorKind::Java);
    }

    #[test]
    fn managed_runtimes_come_first_among_equals() {
        let all = [
            java(17, Arch::X86_64, false, "system"),
            java(17, Arch::X86_64, true, "managed"),
        ];
        let c = choose(&all, 17, Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("managed"));
    }

    #[test]
    fn too_old_runtimes_are_refused() {
        let err = choose(&[java(8, Arch::X86_64, false, "8")], 17, Arch::X86_64).expect_err("old");
        assert_eq!(err.kind, ErrorKind::Java);
        assert!(check_fixed(&java(8, Arch::X86_64, false, "8"), 17, Arch::X86_64).is_err());
        assert!(check_fixed(&java(17, Arch::X86_64, false, "17"), 17, Arch::X86_64).is_ok());
    }
}
