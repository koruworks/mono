// SPDX-License-Identifier: MPL-2.0

//! Is the process on the other end of this connection the Koru app?
//!
//! The question a grant is minted on, answered from the operating system and
//! never from the peer. Two facts are read when a connection asks for a grant:
//!
//! - **which process it is**: the pid the transport reported when the
//!   connection was accepted (`GetNamedPipeClientProcessId` / the socket's
//!   peer credentials), held by the Agent since — not a pid the peer sent;
//! - **which file that process is running**: `proc_pidpath` on macOS,
//!   `/proc/<pid>/exe` on Linux, `QueryFullProcessImageNameW` on Windows.
//!
//! When that file is the `koru` beside this Agent's own executable — the same
//! sibling [`crate::app::koru_beside`] resolves when the Agent launches a
//! window, compared after canonicalising both — the peer is the Koru app of
//! this installation, however it was started: by the Agent or its tray, from
//! Finder, the Dock, the Start menu or the taskbar — before an Agent runs or
//! while one started at sign-in already does — or by `cargo run` beside a
//! `target/debug/koru-agent`. Anything the peer might *say* about itself (an
//! argument, an environment variable, `ClientHello.product`) is never
//! consulted: it is the claim this check exists not to believe. Any lookup
//! that fails is a refusal.
//!
//! ## Pid reuse
//!
//! The pid belongs to a live connection, and the grant is written back on that
//! connection, not to the pid. A process waiting for its answer is alive, so
//! its pid cannot have been reused; one that sent the request and exited has
//! nobody left to read the reply. The pid can only name a different process
//! than the one holding the connection if the original deliberately handed its
//! connection to another process and exited, and a peer doing that is the
//! determined same-user process the [parent module](super) already says this
//! does not stop — it could as easily start the real `koru` and drive it. So
//! there is no creation-time comparison here: it would guard a case that is
//! already conceded, at the cost of carrying an accept time per connection.
//! The Windows read still goes through one open handle, so the image read is
//! the image of the process that handle names.

use std::path::{Path, PathBuf};

use super::Refusal;

/// The operating-system facts the decision needs, so the decision itself is
/// assertable without a second process, a filesystem or a platform.
pub(crate) trait ProcessProbe {
    /// This Agent's own executable, as `spawn` resolves it.
    fn own_exe(&self) -> Option<PathBuf>;
    /// The executable image the OS says `pid` is running.
    fn image_of(&self, pid: u32) -> Option<PathBuf>;
    /// `path` with every symlink and relative step resolved.
    fn canonical(&self, path: &Path) -> Option<PathBuf>;
}

/// A peer the operating system says is running this installation's `koru`.
///
/// Only [`verify`] makes one, and [`super::GrantAuthority::mint`] takes one, so
/// a grant cannot be minted on a path that skipped the check: that is a
/// compile error rather than a test that has to remember to look.
#[derive(Debug)]
pub struct KoruPeer(());

/// Check that `peer_pid` is running the `koru` beside this Agent.
///
/// `peer_pid` is `None` when the platform could not report one, and `None` is
/// a refusal, as is every failed lookup: a check that trusted the caller when
/// it could not check would not be a check.
pub(crate) fn verify(
    peer_pid: Option<u32>,
    probe: &impl ProcessProbe,
) -> Result<KoruPeer, Refusal> {
    let pid = peer_pid.ok_or(Refusal::Unverified)?;
    let expected = probe
        .own_exe()
        .and_then(|own| crate::app::koru_beside(&own))
        .and_then(|koru| probe.canonical(&koru))
        .ok_or(Refusal::Unverified)?;
    let image = probe
        .image_of(pid)
        .and_then(|image| probe.canonical(&image))
        .ok_or(Refusal::Unverified)?;
    if image == expected {
        Ok(KoruPeer(()))
    } else {
        Err(Refusal::NotThisKoru)
    }
}

/// The real probe.
pub(crate) struct Os;

impl ProcessProbe for Os {
    fn own_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
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
    /// ` (deleted)` appended, which then fails to canonicalise — refused: that
    /// window is not running the `koru` that is beside us now.
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

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };

    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: only constructed from a handle this module opened.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// The image of `pid`, read through one handle.
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
            let mut buffer = vec![0u16; 32_768];
            let mut length = buffer.len() as u32;
            if QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) == 0 {
                return None;
            }
            buffer.truncate(length as usize);
            Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer)))
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    pub(super) fn image_of(_pid: u32) -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(test)]
pub(super) mod tests;
