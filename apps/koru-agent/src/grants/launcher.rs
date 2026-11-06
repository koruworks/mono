// SPDX-License-Identifier: MPL-2.0

//! The Agent trusts its launcher: the `koru` window that started this Agent is
//! recorded as if the Agent had started it.
//!
//! The launch table ([`super::record_launch`]) was filled only by the Agent's
//! own `spawn`, which left out the most common window of all. Opened from
//! Finder, the Start menu or a desktop shortcut, `koru` starts *first* and
//! spawns `koru-agent` itself, so the first window of every cold start was a
//! stranger to the Agent: no grant, so Archive and Delete greyed out on the
//! Workspaces page, and a Profile with Workspaces in it could never be removed
//! (ADR-267's 2026-11-06 amendment).
//!
//! The fix keeps the anchor's shape — *the operating system says so* — and
//! points it the other way. Two facts are read, once, at startup, before the
//! Agent serves a single connection:
//!
//! - **who our parent is**: `getppid()` on Unix; on Windows our own entry's
//!   `th32ParentProcessID` in a ToolHelp snapshot;
//! - **which file that parent is running**: `proc_pidpath` on macOS,
//!   `/proc/<pid>/exe` on Linux, `QueryFullProcessImageNameW` on Windows.
//!
//! If that file is the `koru` beside this Agent's own executable — the same
//! sibling [`crate::app::koru_beside`] resolves when the Agent launches a
//! window, compared after canonicalising both — the parent's pid is recorded.
//! Anything the parent might *say* about itself (an argument, an environment
//! variable) is never consulted: it is the claim this mechanism exists not to
//! believe. Any lookup that fails records nothing.
//!
//! ## Pid reuse
//!
//! At the moment this runs the parent is alive — it has just spawned us, and
//! the window that did so is waiting to connect. The reads are still made so a
//! recycled pid cannot slip between them:
//!
//! - **Unix**: the parent is read again *after* its image, and must be
//!   unchanged. A parent that exits reparents us at once, before its pid can be
//!   reaped and reused, so an unchanged `getppid()` means the image we read
//!   belonged to the process that was our parent throughout.
//! - **Windows** does not reparent, so `th32ParentProcessID` can name a pid
//!   whose creator is long gone. The image is read through one open handle —
//!   which keeps the pid from being reused while it is held — and only from a
//!   process created no later than this one: a process younger than us cannot
//!   be the one that started us.
//!
//! What remains is the advisory model the [parent module](super) already
//! states: a same-user process can do much more than this check stops, and
//! after the window exits its pid is in the table until evicted, exactly as a
//! spawned window's is.

use std::path::{Path, PathBuf};

/// The operating-system facts the decision needs, so the decision itself is
/// assertable without a parent process, a filesystem or a platform.
pub(crate) trait LauncherProbe {
    /// This Agent's own executable, as `spawn` resolves it.
    fn own_exe(&self) -> Option<PathBuf>;
    /// The pid of the process that started this one, from the OS.
    fn parent_pid(&self) -> Option<u32>;
    /// The executable image the OS says `pid` is running.
    fn image_of(&self, pid: u32) -> Option<PathBuf>;
    /// `path` with every symlink and relative step resolved.
    fn canonical(&self, path: &Path) -> Option<PathBuf>;
}

/// The pid to trust as this Agent's launcher, or `None`.
///
/// `None` for every failed lookup as well as for a parent that is something
/// else: a check that trusted the caller when it could not check would not be
/// a check.
pub(crate) fn launcher_to_trust(probe: &impl LauncherProbe) -> Option<u32> {
    let own = probe.own_exe()?;
    let expected = probe.canonical(&crate::app::koru_beside(&own)?)?;
    let parent = probe.parent_pid()?;
    // 0 is "no parent" and 1 is init/launchd — what an orphan reads. Neither
    // is ever a window, and asking the OS for their image is pointless.
    if parent <= 1 {
        return None;
    }
    let image = probe.canonical(&probe.image_of(parent)?)?;
    // Still our parent after the read: see "Pid reuse" in the module docs.
    if probe.parent_pid()? != parent {
        return None;
    }
    (image == expected).then_some(parent)
}

/// Record the launcher in `authority`, if it is the `koru` beside us.
pub(crate) fn trust_launcher_in(
    authority: &mut super::GrantAuthority,
    probe: &impl LauncherProbe,
    now_millis: i64,
) -> Option<u32> {
    let pid = launcher_to_trust(probe)?;
    authority.launched(pid, now_millis);
    Some(pid)
}

/// Called once from [`crate::run::run`], after the endpoint is bound and
/// before the accept loop starts, so the launcher's first request already
/// finds its pid in the table.
pub fn trust_launcher() {
    // Through the same function the tests drive, so what they assert is what
    // runs. The lock is held across the OS reads, which costs nothing here:
    // nothing else can be asking for a grant before the accept loop starts.
    match super::with(|authority| trust_launcher_in(authority, &Os, super::now_millis())) {
        Some(pid) => {
            tracing::info!(pid, "agent: trusting the Koru window that started it");
        }
        None => tracing::debug!("agent: not started by the Koru beside it"),
    }
}

/// The real probe.
struct Os;

impl LauncherProbe for Os {
    fn own_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }

    fn parent_pid(&self) -> Option<u32> {
        platform::parent_pid()
    }

    fn image_of(&self, pid: u32) -> Option<PathBuf> {
        platform::image_of(pid)
    }

    fn canonical(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }
}

#[cfg(unix)]
mod platform {
    use std::path::PathBuf;

    pub(super) fn parent_pid() -> Option<u32> {
        Some(std::os::unix::process::parent_id())
    }

    #[cfg(target_os = "macos")]
    pub(super) fn image_of(pid: u32) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStringExt;
        let pid = libc::pid_t::try_from(pid).ok()?;
        let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is writable for exactly the length passed, and
        // the call writes at most that many bytes.
        let written =
            unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        if written <= 0 {
            return None;
        }
        buffer.truncate(written as usize);
        Some(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
    }

    /// The kernel's own answer. A binary replaced since it started reads with
    /// ` (deleted)` appended, which then fails to canonicalise — refused, as
    /// it should be: that window is not running the `koru` beside us.
    #[cfg(target_os = "linux")]
    pub(super) fn image_of(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn image_of(_pid: u32) -> Option<PathBuf> {
        None
    }
}

#[cfg(windows)]
mod platform {
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };

    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: only constructed from a handle this module opened.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Our own row's parent in a process snapshot. The ToolHelp binding is
    /// the one `update::kill_mcp_bridges` already uses.
    pub(super) fn parent_pid() -> Option<u32> {
        let own = std::process::id();
        // SAFETY: the snapshot handle is checked and closed by `Owned`; the
        // entry is sized as the API requires before the first call.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return None;
            }
            let snapshot = Owned(snapshot);
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snapshot.0, &mut entry) != 0;
            while more {
                if entry.th32ProcessID == own {
                    return Some(entry.th32ParentProcessID).filter(|pid| *pid != 0);
                }
                more = Process32NextW(snapshot.0, &mut entry) != 0;
            }
            None
        }
    }

    /// The image of `pid`, read through one handle, and only when that
    /// process is no younger than this one — see "Pid reuse".
    pub(super) fn image_of(pid: u32) -> Option<PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        // SAFETY: the handle is checked and closed by `Owned`; the buffer is
        // writable for the length passed, which the call updates in place.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let handle = Owned(handle);
            if created(handle.0)? > created(GetCurrentProcess())? {
                return None;
            }
            let mut buffer = vec![0u16; 32_768];
            let mut length = buffer.len() as u32;
            if QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) == 0 {
                return None;
            }
            buffer.truncate(length as usize);
            Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer)))
        }
    }

    /// When `process` was created, in FILETIME ticks.
    unsafe fn created(process: HANDLE) -> Option<u64> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: four writable FILETIMEs and a handle the caller holds open.
        let ok =
            unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
        (ok != 0)
            .then(|| (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    pub(super) fn parent_pid() -> Option<u32> {
        None
    }

    pub(super) fn image_of(_pid: u32) -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests;
