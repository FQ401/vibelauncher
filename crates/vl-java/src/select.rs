//! Choosing a Java runtime for a game version.
//!
//! A game version asks for a runtime through its version JSON (`javaVersion.component` and
//! `majorVersion`); a version without that field runs on Java 8 ([`JavaRequirement::legacy`]).
//! A runtime the user picked explicitly always wins; otherwise the best installed runtime is
//! chosen, and when none fits the caller is told what to download.

use vl_core::java::JavaRuntime;
use vl_core::platform::Arch;

/// What a game version needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaRequirement {
    /// The Mojang runtime component (`jre-legacy`, `java-runtime-delta`); empty when only the
    /// major version is known.
    pub component: String,
    /// The Java major version.
    pub major: u32,
}

impl JavaRequirement {
    /// A requirement from a component name and a major version.
    pub fn new(component: impl Into<String>, major: u32) -> Self {
        Self {
            component: component.into(),
            major,
        }
    }

    /// A requirement for a major version only.
    pub fn from_major(major: u32) -> Self {
        Self::new("", major)
    }

    /// What a version without `javaVersion` runs on: Java 8.
    pub fn legacy() -> Self {
        Self::new("jre-legacy", 8)
    }
}

/// Whether `runtime` can run a game that needs `requirement` on a machine of architecture `host`.
///
/// Java 8 games need exactly Java 8 (newer runtimes break the old class loader); newer games run
/// on their major version or any later one. The runtime must match the machine's architecture, so
/// a 32-bit Java on a 64-bit machine does not qualify.
pub fn satisfies(runtime: &JavaRuntime, requirement: &JavaRequirement, host: Arch) -> bool {
    let arch_ok = host == Arch::Other || runtime.arch == host;
    arch_ok && major_fits(runtime.major, requirement.major)
}

/// Whether a runtime of major version `have` runs a game that needs `want`: exactly 8 for Java 8
/// games, `want` or later otherwise.
pub fn major_fits(have: u32, want: u32) -> bool {
    if want <= 8 { have == 8 } else { have >= want }
}

/// The best runtime among `runtimes` for `requirement`: the closest major version, then managed
/// before system, then the first in list order. `None` when none qualifies.
pub fn choose<'a>(
    runtimes: &'a [JavaRuntime],
    requirement: &JavaRequirement,
    host: Arch,
) -> Option<&'a JavaRuntime> {
    runtimes
        .iter()
        .filter(|r| satisfies(r, requirement, host))
        .enumerate()
        .min_by_key(|(i, r)| (r.major.abs_diff(requirement.major), !r.managed, *i))
        .map(|(_, r)| r)
}

/// The outcome of [`resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Run the game with this runtime.
    Use(JavaRuntime),
    /// No installed runtime fits; offer to download this one.
    Download(JavaRequirement),
}

/// Picks the runtime for a launch. `explicit` is the user's own choice (an instance or global
/// setting); it is used as it is, whatever its version, and the caller may warn when
/// [`satisfies`] is false.
pub fn resolve(
    runtimes: &[JavaRuntime],
    requirement: &JavaRequirement,
    host: Arch,
    explicit: Option<&JavaRuntime>,
) -> Resolution {
    if let Some(runtime) = explicit {
        return Resolution::Use(runtime.clone());
    }
    match choose(runtimes, requirement, host) {
        Some(runtime) => Resolution::Use(runtime.clone()),
        None => Resolution::Download(requirement.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn rt(name: &str, major: u32, arch: Arch, managed: bool) -> JavaRuntime {
        JavaRuntime {
            path: PathBuf::from(name),
            major,
            version: major.to_string(),
            arch,
            vendor: "Synthetic".into(),
            managed,
        }
    }

    const X64: Arch = Arch::X86_64;

    #[test]
    fn java_8_games_need_exactly_java_8() {
        let list = [rt("a", 17, X64, false), rt("b", 21, X64, false)];
        assert_eq!(choose(&list, &JavaRequirement::legacy(), X64), None);
        let list = [rt("a", 17, X64, false), rt("b", 8, X64, false)];
        assert_eq!(
            choose(&list, &JavaRequirement::legacy(), X64).map(|r| r.path.clone()),
            Some(PathBuf::from("b"))
        );
    }

    #[test]
    fn newer_games_take_the_closest_major_at_or_above() {
        let list = [
            rt("8", 8, X64, false),
            rt("25", 25, X64, false),
            rt("21", 21, X64, false),
            rt("17", 17, X64, false),
        ];
        let want = |major, expect: Option<&str>| {
            let got = choose(&list, &JavaRequirement::new("c", major), X64);
            assert_eq!(got.map(|r| r.path.to_str().unwrap_or_default()), expect);
        };
        want(17, Some("17"));
        want(18, Some("21"));
        want(21, Some("21"));
        want(22, Some("25"));
        want(26, None);
    }

    #[test]
    fn managed_wins_a_tie_and_the_architecture_must_match() {
        let list = [
            rt("system", 21, X64, false),
            rt("managed", 21, X64, true),
            rt("small", 21, Arch::X86, true),
            rt("arm", 21, Arch::Aarch64, true),
        ];
        let req = JavaRequirement::from_major(21);
        assert_eq!(
            choose(&list, &req, X64).map(|r| r.path.clone()),
            Some(PathBuf::from("managed"))
        );
        let only_small = [rt("small", 8, Arch::X86, false)];
        assert_eq!(choose(&only_small, &JavaRequirement::legacy(), X64), None);
        assert!(choose(&only_small, &JavaRequirement::legacy(), Arch::Other).is_some());
    }

    #[test]
    fn an_explicit_choice_wins_and_a_missing_runtime_asks_for_a_download() {
        let list = [rt("system", 21, X64, false)];
        let old = rt("mine", 8, X64, false);
        let req = JavaRequirement::from_major(21);
        assert_eq!(
            resolve(&list, &req, X64, Some(&old)),
            Resolution::Use(old.clone())
        );
        assert!(!satisfies(&old, &req, X64));
        assert_eq!(
            resolve(&list, &req, X64, None),
            Resolution::Use(list[0].clone())
        );
        assert_eq!(
            resolve(&[], &req, X64, None),
            Resolution::Download(req.clone())
        );
    }
}
