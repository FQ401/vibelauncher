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

/// The newest Java that games asking for Java 8 or older run on.
const LEGACY_MAJOR: u32 = 8;

fn arch_fits(runtime: &JavaRuntime, wanted: Arch) -> bool {
    wanted == Arch::Other || runtime.arch == wanted
}

fn no_java(required: u32, max: Option<u32>, wanted: Arch) -> Error {
    let range = match max {
        Some(max) if max == required => format!("Java {required}"),
        Some(max) => format!("Java {required} to {max}"),
        None => format!("Java {required} or newer"),
    };
    Error::new(
        ErrorKind::Java,
        format!("no {range} for {wanted:?} was found"),
    )
}

/// The best of `runtimes` for a game version that needs Java `required_major` (and, for a
/// version that breaks on newer runtimes, at most `max_major`) on architecture `arch`.
///
/// The exact major version comes first, else the lowest newer one; managed runtimes before
/// others; the first of equals in the given order. A game that needs Java 8 or older runs on
/// exactly Java 8 (Forge 1.16.5 and older break on newer runtimes, whatever their launch
/// wrapper), and also accepts a 32-bit Java on a 64-bit machine as a last resort: its natives
/// archives carry both word sizes.
///
/// # Errors
///
/// `Java` when no runtime has the architecture and a version in range.
pub fn choose(
    runtimes: &[JavaRuntime],
    required_major: u32,
    max_major: Option<u32>,
    arch: Arch,
) -> Result<JavaChoice> {
    let max_major = if required_major <= LEGACY_MAJOR {
        Some(LEGACY_MAJOR)
    } else {
        max_major
    };
    let in_range =
        |r: &&JavaRuntime| r.major >= required_major && max_major.is_none_or(|max| r.major <= max);
    let best = |fits: &dyn Fn(&JavaRuntime) -> bool| {
        runtimes
            .iter()
            .filter(in_range)
            .filter(|r| fits(r))
            .min_by_key(|r| (r.major, !r.managed))
    };
    best(&|r| arch_fits(r, arch))
        .or_else(|| {
            let legacy = required_major <= 8 && arch == Arch::X86_64;
            if legacy {
                best(&|r| r.arch == Arch::X86)
            } else {
                None
            }
        })
        .map(|r| JavaChoice {
            exact: r.major == required_major,
            runtime: r.clone(),
        })
        .ok_or_else(|| no_java(required_major, max_major, arch))
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
        let c = choose(&all, 17, None, Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("17"));
        assert!(c.exact);
        let c = choose(&all[..2], 16, None, Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.major, 17);
        assert!(!c.exact);
    }

    #[test]
    fn architecture_must_fit() {
        let all = [
            java(8, Arch::X86, false, "8-32"),
            java(8, Arch::X86_64, false, "8-64"),
        ];
        let c = choose(&all, 8, None, Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("8-64"));
        // A 32-bit Java 8 is a last resort for a game that needs Java 8, and for no other.
        let c = choose(&all[..1], 8, None, Arch::X86_64).expect("legacy fallback");
        assert_eq!(c.runtime.path, PathBuf::from("8-32"));
        let modern = [java(17, Arch::X86, false, "17-32")];
        let err = choose(&modern, 17, None, Arch::X86_64).expect_err("no fit");
        assert_eq!(err.kind, ErrorKind::Java);
    }

    #[test]
    fn a_maximum_keeps_old_games_off_new_runtimes() {
        let all = [
            java(21, Arch::X86_64, false, "21"),
            java(8, Arch::X86_64, false, "8"),
        ];
        let c = choose(&all, 8, Some(8), Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.major, 8);
        let err = choose(&all[..1], 8, Some(8), Arch::X86_64).expect_err("too new");
        assert_eq!(err.kind, ErrorKind::Java);
        assert!(err.message.contains("Java 8 "), "{}", err.message);
    }

    #[test]
    fn java_8_games_never_get_a_newer_runtime() {
        // Forge 1.16.5: the vanilla version JSON asks for Java 8 and the launch wrapper is
        // not `launchwrapper`, so the caller passes no maximum.
        let newer = [
            java(21, Arch::X86_64, false, "21"),
            java(17, Arch::X86_64, true, "17"),
        ];
        let err = choose(&newer, 8, None, Arch::X86_64).expect_err("too new");
        assert_eq!(err.kind, ErrorKind::Java);
        let mut all = newer.to_vec();
        all.push(java(8, Arch::X86_64, false, "8"));
        assert_eq!(
            choose(&all, 8, None, Arch::X86_64).unwrap().runtime.major,
            8
        );
        // Older requirements (1.12.2 and below without `javaVersion`) are Java 8 as well.
        assert_eq!(
            choose(&all, 6, None, Arch::X86_64).unwrap().runtime.major,
            8
        );
        // 1.21 asks for 21.
        assert_eq!(
            choose(&all, 21, None, Arch::X86_64).unwrap().runtime.major,
            21
        );
    }

    #[test]
    fn managed_runtimes_come_first_among_equals() {
        let all = [
            java(17, Arch::X86_64, false, "system"),
            java(17, Arch::X86_64, true, "managed"),
        ];
        let c = choose(&all, 17, None, Arch::X86_64).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("managed"));
    }

    #[test]
    fn too_old_runtimes_are_refused() {
        let err =
            choose(&[java(8, Arch::X86_64, false, "8")], 17, None, Arch::X86_64).expect_err("old");
        assert_eq!(err.kind, ErrorKind::Java);
        assert!(check_fixed(&java(8, Arch::X86_64, false, "8"), 17, Arch::X86_64).is_err());
        assert!(check_fixed(&java(17, Arch::X86_64, false, "17"), 17, Arch::X86_64).is_ok());
    }
}
