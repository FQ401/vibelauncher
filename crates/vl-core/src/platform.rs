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
