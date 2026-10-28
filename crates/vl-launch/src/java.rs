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
    /// One line for the launch log: which runtime was chosen and why.
    pub reason: String,
}

/// The newest Java that games asking for Java 8 or older run on.
const LEGACY_MAJOR: u32 = 8;

/// The largest heap a 32-bit Java is trusted with; above this the game needs a 64-bit Java.
pub const MAX_HEAP_32_BIT_MIB: u32 = 1024;

fn arch_fits(runtime: &JavaRuntime, wanted: Arch) -> bool {
    wanted == Arch::Other || runtime.arch == wanted
}

fn is_32_bit_on_64(runtime: &JavaRuntime, wanted: Arch) -> bool {
    wanted == Arch::X86_64 && runtime.arch == Arch::X86
}

fn range_text(required: u32, max: Option<u32>) -> String {
    match max {
        Some(max) if max == required => format!("Java {required}"),
        Some(max) => format!("Java {required} to {max}"),
        None => format!("Java {required} or newer"),
    }
}

fn no_java(required: u32, max: Option<u32>, wanted: Arch, small_only: Option<u32>) -> Error {
    let range = range_text(required, max);
    let message = match small_only {
        Some(heap) => format!(
            "only a 32-bit {range} was found, which cannot hold a {heap} MiB heap; a {wanted:?} Java is needed"
        ),
        None => format!("no {range} for {wanted:?} was found"),
    };
    Error::new(ErrorKind::Java, message)
}

fn describe(runtime: &JavaRuntime) -> String {
    format!(
        "Java {} ({:?}, {}, {})",
        runtime.version,
        runtime.arch,
        if runtime.managed { "managed" } else { "system" },
        runtime.path.display()
    )
}

/// The best of `runtimes` for a game version that needs Java `required_major` (and, for a
/// version that breaks on newer runtimes, at most `max_major`) on architecture `arch`, started
/// with a heap of up to `heap_mib`.
///
/// The machine's own architecture comes first, then the exact major version, else the lowest
/// newer one; managed runtimes before others; the first of equals in the given order. A game
/// that needs Java 8 or older runs on exactly Java 8 (Forge 1.16.5 and older break on newer
/// runtimes, whatever their launch wrapper). A 32-bit Java on a 64-bit machine is a last resort
/// for such a game and only when the heap fits in [`MAX_HEAP_32_BIT_MIB`]: a 32-bit JVM cannot
/// address more, and its natives archives carry both word sizes.
///
/// # Errors
///
/// `Java` when no runtime has the architecture and a version in range.
pub fn choose(
    runtimes: &[JavaRuntime],
    required_major: u32,
    max_major: Option<u32>,
    arch: Arch,
    heap_mib: u32,
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
    let wanted_range = range_text(required_major, max_major);
    let (found, why) = if let Some(r) = best(&|r| arch_fits(r, arch)) {
        (
            Some(r),
            format!("matches the machine ({arch:?}) and {wanted_range}"),
        )
    } else if required_major <= LEGACY_MAJOR && heap_mib <= MAX_HEAP_32_BIT_MIB {
        (
            best(&|r| is_32_bit_on_64(r, arch)),
            format!(
                "no {arch:?} {wanted_range} is installed; a 32-bit one is enough for a                  {heap_mib} MiB heap"
            ),
        )
    } else {
        (None, String::new())
    };
    match found {
        Some(r) => Ok(JavaChoice {
            exact: r.major == required_major,
            reason: format!("using {}: {why}", describe(r)),
            runtime: r.clone(),
        }),
        None => {
            let small_only = (required_major <= LEGACY_MAJOR
                && heap_mib > MAX_HEAP_32_BIT_MIB
                && best(&|r| is_32_bit_on_64(r, arch)).is_some())
            .then_some(heap_mib);
            Err(no_java(required_major, max_major, arch, small_only))
        }
    }
}

/// Checks a runtime the user fixed in the settings: it must be new enough and fit the
/// architecture. A 32-bit Java on a 64-bit machine is respected, with a note in the reason that
/// it limits the heap.
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
    if !arch_fits(runtime, arch) && !is_32_bit_on_64(runtime, arch) {
        return Err(Error::new(
            ErrorKind::Java,
            format!(
                "the selected Java is {:?} but the game needs {arch:?}",
                runtime.arch
            ),
        ));
    }
    let note = if is_32_bit_on_64(runtime, arch) {
        format!(
            "; it is 32-bit on a {arch:?} machine, so the heap is limited to              {MAX_HEAP_32_BIT_MIB} MiB"
        )
    } else {
        String::new()
    };
    Ok(JavaChoice {
        exact: runtime.major == required_major,
        reason: format!("using {}: chosen in the settings{note}", describe(runtime)),
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

    const X64: Arch = Arch::X86_64;
    const HEAP: u32 = 4096;

    fn pick(all: &[JavaRuntime], major: u32, arch: Arch, heap: u32) -> Result<JavaChoice> {
        choose(all, major, None, arch, heap)
    }

    #[test]
    fn exact_major_wins_then_the_lowest_newer() {
        let all = [
            java(21, X64, false, "21"),
            java(17, X64, false, "17"),
            java(8, X64, false, "8"),
        ];
        let c = pick(&all, 17, X64, HEAP).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("17"));
        assert!(c.exact);
        let c = pick(&all[..2], 16, X64, HEAP).expect("choice");
        assert_eq!(c.runtime.major, 17);
        assert!(!c.exact);
    }

    #[test]
    fn x86_and_x64_java_8_prefers_x64() {
        let all = [
            java(8, Arch::X86, false, "8-32"),
            java(8, X64, false, "8-64"),
        ];
        let c = pick(&all, 8, X64, HEAP).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("8-64"));
        assert!(c.reason.contains("matches the machine"), "{}", c.reason);
        // Even a managed 32-bit one loses to a system 64-bit one.
        let all = [
            java(8, Arch::X86, true, "8-32"),
            java(8, X64, false, "8-64"),
        ];
        assert_eq!(
            pick(&all, 8, X64, 512).unwrap().runtime.path,
            PathBuf::from("8-64")
        );
    }

    #[test]
    fn only_x86_java_8_is_for_small_heaps_only() {
        let only = [java(8, Arch::X86, false, "8-32")];
        let c = pick(&only, 8, X64, 1024).expect("small heap");
        assert_eq!(c.runtime.path, PathBuf::from("8-32"));
        assert!(c.reason.contains("32-bit"), "{}", c.reason);
        let err = pick(&only, 8, X64, 1025).expect_err("heap too large");
        assert_eq!(err.kind, ErrorKind::Java);
        assert!(err.message.contains("32-bit"), "{}", err.message);
        // Never for newer games.
        let modern = [java(17, Arch::X86, false, "17-32")];
        assert!(pick(&modern, 17, X64, 512).is_err());
    }

    #[test]
    fn only_x64_java_8_is_used() {
        let only = [java(8, X64, false, "8-64")];
        assert_eq!(
            pick(&only, 8, X64, HEAP).unwrap().runtime.path,
            PathBuf::from("8-64")
        );
    }

    #[test]
    fn arm64_wants_arm64_and_nothing_else() {
        let arm = Arch::Aarch64;
        let all = [java(8, X64, false, "8-x64"), java(8, arm, false, "8-arm")];
        assert_eq!(
            pick(&all, 8, arm, HEAP).unwrap().runtime.path,
            PathBuf::from("8-arm")
        );
        // The x64 one needs the platform to be adjusted to x64 first (Rosetta).
        assert!(pick(&all[..1], 8, arm, 512).is_err());
        assert_eq!(
            pick(&all, 8, X64, HEAP).unwrap().runtime.path,
            PathBuf::from("8-x64")
        );
        let x86 = [java(8, Arch::X86, false, "8-32")];
        assert!(pick(&x86, 8, arm, 512).is_err());
    }

    #[test]
    fn a_maximum_keeps_old_games_off_new_runtimes() {
        let all = [java(21, X64, false, "21"), java(8, X64, false, "8")];
        let c = choose(&all, 8, Some(8), X64, HEAP).expect("choice");
        assert_eq!(c.runtime.major, 8);
        let err = choose(&all[..1], 8, Some(8), X64, HEAP).expect_err("too new");
        assert_eq!(err.kind, ErrorKind::Java);
        assert!(err.message.contains("Java 8 "), "{}", err.message);
    }

    #[test]
    fn java_8_games_never_get_a_newer_runtime() {
        let newer = [java(21, X64, false, "21"), java(17, X64, true, "17")];
        let err = pick(&newer, 8, X64, HEAP).expect_err("too new");
        assert_eq!(err.kind, ErrorKind::Java);
        let mut all = newer.to_vec();
        all.push(java(8, X64, false, "8"));
        assert_eq!(pick(&all, 8, X64, HEAP).unwrap().runtime.major, 8);
        assert_eq!(pick(&all, 6, X64, HEAP).unwrap().runtime.major, 8);
        assert_eq!(pick(&all, 21, X64, HEAP).unwrap().runtime.major, 21);
    }

    #[test]
    fn managed_runtimes_come_first_among_equals() {
        let all = [
            java(17, Arch::X86_64, false, "system"),
            java(17, Arch::X86_64, true, "managed"),
        ];
        let c = pick(&all, 17, X64, HEAP).expect("choice");
        assert_eq!(c.runtime.path, PathBuf::from("managed"));
    }

    #[test]
    fn too_old_runtimes_are_refused() {
        let err = pick(&[java(8, X64, false, "8")], 17, X64, HEAP).expect_err("old");
        assert_eq!(err.kind, ErrorKind::Java);
        assert!(check_fixed(&java(8, X64, false, "8"), 17, X64).is_err());
        assert!(check_fixed(&java(17, X64, false, "17"), 17, X64).is_ok());
    }

    #[test]
    fn a_fixed_32_bit_java_is_respected_with_a_note() {
        let small = java(8, Arch::X86, false, "8-32");
        let c = check_fixed(&small, 8, X64).expect("respected");
        assert!(c.reason.contains("settings"), "{}", c.reason);
        assert!(c.reason.contains("limited"), "{}", c.reason);
        // Another architecture is still an error.
        assert!(check_fixed(&java(8, Arch::Aarch64, false, "arm"), 8, X64).is_err());
    }
}
