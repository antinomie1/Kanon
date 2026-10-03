//! Cross-platform host process introspection for the management console.
//!
//! Memory figures are sampled on demand through `sysinfo` so the same code path works on
//! Linux, macOS and Windows. Sampling failures are reported as `None` rather than being
//! coerced to zero, because a fabricated `0 bytes` reading would silently mislead operators.

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Resident and virtual memory consumption of the core process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MemoryUsage {
    /// Resident set size in bytes (physical memory currently held).
    pub resident_bytes: u64,
    /// Virtual address space size in bytes.
    pub virtual_bytes: u64,
}

/// Samples the current process memory footprint.
///
/// Returns `None` when the operating system refuses to report the process — e.g. the
/// process vanished from the process table or the platform lacks the required permission.
pub fn sample_process_memory() -> Option<MemoryUsage> {
    let pid = Pid::from_u32(std::process::id());
    let mut system = System::new();

    // The console polls this endpoint, so collect only this process's memory. The default
    // process refresh also reads CPU, disk I/O and executable data that we never expose.
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );

    system.process(pid).map(|process| MemoryUsage {
        resident_bytes: process.memory(),
        virtual_bytes: process.virtual_memory(),
    })
}
