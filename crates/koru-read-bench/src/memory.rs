// SPDX-License-Identifier: MPL-2.0

//! **This process's memory, from the operating system's own counters.**
//!
//! On Windows, private committed bytes (`PagefileUsage`) and their lifetime
//! peak (`PeakPagefileUsage`): what Plan 0.1 measured, and what a Job Object
//! memory limit — the worker's 1 GiB ceiling — counts. Elsewhere, the peak
//! resident set from `getrusage`, which is a different quantity and is
//! labelled so. A peak is the OS's own, never a sample: a sampled maximum
//! misses the spike between samples, and a page's peak is exactly a spike.

/// What a counter measures.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Measure {
    PrivateCommitted,
    ResidentSet,
}

impl Measure {
    pub fn label(self) -> &'static str {
        match self {
            Measure::PrivateCommitted => "private committed",
            Measure::ResidentSet => "resident set",
        }
    }
}

/// `(now, lifetime peak)` in bytes, and what they measure; `None` where the
/// platform will not say.
pub fn read() -> Option<(u64, u64, Measure)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: the struct is zeroed and its `cb` is its own size, which is
        // the whole of this API's contract; the pseudo-handle needs no
        // closing.
        unsafe {
            let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) != 0 {
                return Some((
                    counters.PagefileUsage as u64,
                    counters.PeakPagefileUsage as u64,
                    Measure::PrivateCommitted,
                ));
            }
        }
        None
    }
    #[cfg(unix)]
    {
        // SAFETY: `getrusage` writes into a zeroed struct we own.
        unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
                return None;
            }
            let raw = usage.ru_maxrss as u64;
            // macOS reports bytes, Linux kibibytes.
            let peak = if cfg!(target_os = "macos") {
                raw
            } else {
                raw * 1024
            };
            Some((0, peak, Measure::ResidentSet))
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

/// Bytes as MiB, one decimal.
pub fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}
