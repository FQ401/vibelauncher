//! The memory check before a launch.

use std::fmt;

/// Physical memory of the machine, in MiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryStatus {
    /// Installed memory.
    pub total_mib: u64,
    /// Memory that can be given to a new process without swapping.
    pub available_mib: u64,
}

/// The requested heap does not fit into the free memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryWarning {
    /// The requested maximum heap, MiB.
    pub requested_mib: u64,
    /// Available memory, MiB.
    pub available_mib: u64,
}

impl fmt::Display for MemoryWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the game may run out of memory: {} MiB requested, {} MiB available",
            self.requested_mib, self.available_mib
        )
    }
}

/// Reads the memory of this machine; `None` when the OS does not report it.
pub fn status() -> Option<MemoryStatus> {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    let total = system.total_memory();
    if total == 0 {
        return None;
    }
    let mib = 1024 * 1024;
    Some(MemoryStatus {
        total_mib: total / mib,
        available_mib: system.available_memory() / mib,
    })
}

/// A warning when `max_mib` is more than is available. Without a status there is nothing to
/// compare with and no warning.
pub fn check(max_mib: u32, status: Option<MemoryStatus>) -> Option<MemoryWarning> {
    let status = status?;
    (u64::from(max_mib) > status.available_mib).then_some(MemoryWarning {
        requested_mib: u64::from(max_mib),
        available_mib: status.available_mib,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_only_when_the_heap_exceeds_the_available_memory() {
        let status = Some(MemoryStatus {
            total_mib: 16384,
            available_mib: 3000,
        });
        assert!(check(2048, status).is_none());
        assert!(check(3000, status).is_none());
        let warning = check(4096, status).expect("warning");
        assert_eq!(warning.available_mib, 3000);
        assert!(warning.to_string().contains("4096"));
        assert!(check(4096, None).is_none());
    }

    #[test]
    fn this_machine_reports_memory() {
        let status = status().expect("memory status");
        assert!(status.total_mib > 0);
    }
}
