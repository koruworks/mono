// SPDX-License-Identifier: MPL-2.0

//! Which process started this Agent — its pid and, where the platform says,
//! its image name — for the one line the Agent logs when it starts.
//!
//! An Agent is started by whoever notices there is none: a window, the MCP
//! bridge, the login item, the updater, a developer's launcher. When several
//! start and stop within seconds of each other, the log said *that* it
//! happened and nothing about *who*, and the cause was guessed from timing.
//!
//! The name is what the parent pid is running **now**. A parent that has
//! already exited leaves a pid the system may have handed to something else,
//! so an exited parent reads as `gone` rather than as a stranger's name only
//! when the pid is no longer listed at all; treat the name as a strong hint,
//! not as proof.

/// The parent's pid and image name. `0` and `unknown` when the platform will
/// not say; `gone` for a parent that is no longer running.
#[cfg(windows)]
pub fn launched_by() -> (u32, String) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let own = std::process::id();
    let mut processes: Vec<(u32, u32, String)> = Vec::new();
    // SAFETY: the snapshot handle is checked before use and closed once; the
    // entry is a plain-old-data struct whose size field is set as the API
    // requires, and only the bytes the API wrote before the NUL are read.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return (0, "unknown".into());
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            processes.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                String::from_utf16_lossy(&entry.szExeFile[..len]),
            ));
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    name_the_parent(own, &processes)
}

#[cfg(unix)]
pub fn launched_by() -> (u32, String) {
    (std::os::unix::process::parent_id(), "unknown".into())
}

#[cfg(not(any(windows, unix)))]
pub fn launched_by() -> (u32, String) {
    (0, "unknown".into())
}

/// Pick this process's parent out of a process listing of
/// `(pid, parent pid, image name)`.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn name_the_parent(own: u32, processes: &[(u32, u32, String)]) -> (u32, String) {
    let Some(parent) = processes
        .iter()
        .find(|(pid, _, _)| *pid == own)
        .map(|(_, parent, _)| *parent)
    else {
        return (0, "unknown".into());
    };
    let name = processes
        .iter()
        .find(|(pid, _, _)| *pid == parent)
        .map_or_else(|| "gone".to_owned(), |(_, _, name)| name.clone());
    (parent, name)
}

#[cfg(test)]
mod tests {
    use super::name_the_parent;

    fn listing() -> Vec<(u32, u32, String)> {
        vec![
            (4, 0, "System".into()),
            (100, 4, "koru.exe".into()),
            (200, 100, "koru-agent.exe".into()),
            (300, 999, "koru-agent.exe".into()),
        ]
    }

    #[test]
    fn the_parent_is_named_from_the_listing() {
        assert_eq!(name_the_parent(200, &listing()), (100, "koru.exe".into()));
    }

    /// A launcher that started the Agent and left is the common case for a
    /// detached spawn, and must not be reported as nothing at all.
    #[test]
    fn a_parent_that_has_exited_keeps_its_pid_and_reads_as_gone() {
        assert_eq!(name_the_parent(300, &listing()), (999, "gone".into()));
    }

    #[test]
    fn a_process_missing_from_its_own_listing_is_unknown() {
        assert_eq!(name_the_parent(7, &listing()), (0, "unknown".into()));
    }
}
