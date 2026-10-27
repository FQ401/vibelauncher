//! The operating system and CPU architecture that metadata rules and Java runtimes refer to.

use serde::{Deserialize, Serialize};

/// An operating system the launcher knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    /// Windows.
    Windows,
    /// macOS.
    MacOs,
    /// Linux.
    Linux,
    /// Any other OS (FreeBSD, the browser).
    Other,
}

impl Os {
    /// The name Mojang metadata uses for this OS (`windows`, `osx`, `linux`); `None` for
    /// [`Os::Other`].
    pub const fn mojang_name(self) -> Option<&'static str> {
        match self {
            Os::Windows => Some("windows"),
            Os::MacOs => Some("osx"),
            Os::Linux => Some("linux"),
            Os::Other => None,
        }
    }

    /// The OS a Mojang name (`windows`, `osx`, `linux`) denotes; `None` for any other name.
    pub fn from_mojang_name(name: &str) -> Option<Self> {
        match name {
            "windows" => Some(Os::Windows),
            "osx" => Some(Os::MacOs),
            "linux" => Some(Os::Linux),
            _ => None,
        }
    }
}

/// A CPU architecture the launcher knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    /// 32-bit x86.
    X86,
    /// 64-bit x86.
    X86_64,
    /// 64-bit ARM.
    Aarch64,
    /// 32-bit ARM.
    Arm,
    /// Any other architecture.
    Other,
}

impl Arch {
    /// The architecture a Mojang `os.arch` value denotes: `x86`, `x86_64`/`amd64`,
    /// `arm64`/`aarch64`, `arm`. `None` for any other value.
    pub fn from_mojang_name(name: &str) -> Option<Self> {
        match name {
            "x86" | "i386" | "i686" => Some(Arch::X86),
            "x86_64" | "amd64" => Some(Arch::X86_64),
            "arm64" | "aarch64" => Some(Arch::Aarch64),
            "arm" => Some(Arch::Arm),
            _ => None,
        }
    }

    /// The pointer width in bits (`32` or `64`), the value of `${arch}` in natives classifiers;
    /// `None` for [`Arch::Other`].
    pub const fn bits(self) -> Option<u32> {
        match self {
            Arch::X86 | Arch::Arm => Some(32),
            Arch::X86_64 | Arch::Aarch64 => Some(64),
            Arch::Other => None,
        }
    }
}

/// An OS and an architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    /// The operating system.
    pub os: Os,
    /// The CPU architecture.
    pub arch: Arch,
}

impl Platform {
    /// The platform this binary was built for.
    pub const fn current() -> Self {
        let os = if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            Os::Other
        };
        let arch = if cfg!(target_arch = "x86") {
            Arch::X86
        } else if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Arch::Aarch64
        } else if cfg!(target_arch = "arm") {
            Arch::Arm
        } else {
            Arch::Other
        };
        Self { os, arch }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mojang_names_round_trip() {
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            let name = os.mojang_name().unwrap();
            assert_eq!(Os::from_mojang_name(name), Some(os));
        }
        assert_eq!(Os::Other.mojang_name(), None);
        assert_eq!(Os::from_mojang_name("freebsd"), None);
    }

    #[test]
    fn arch_names_and_bits() {
        assert_eq!(Arch::from_mojang_name("arm64"), Some(Arch::Aarch64));
        assert_eq!(Arch::from_mojang_name("amd64"), Some(Arch::X86_64));
        assert_eq!(Arch::from_mojang_name("x86"), Some(Arch::X86));
        assert_eq!(Arch::from_mojang_name("riscv"), None);
        assert_eq!(Arch::Aarch64.bits(), Some(64));
        assert_eq!(Arch::X86.bits(), Some(32));
        assert_eq!(Arch::Other.bits(), None);
    }
}
