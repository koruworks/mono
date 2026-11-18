// SPDX-License-Identifier: MPL-2.0

//! **Whether the system is saving energy** — the one question a reading
//! backlog asks before its next file (OCR TECHNICAL §14 "Staying out of the
//! way": "A backlog pauses under battery saver; a fresh file does not").
//!
//! A seam, as the launcher and the objects are: the coordinator is handed a
//! [`Power`], tests hand it one they set, and only the daemon's composition
//! hands it [`SystemPower`]. **No test reads the machine's power state.**
//!
//! ## Per platform
//!
//! - **Windows**: `GetSystemPowerStatus`, whose `SystemStatusFlag` is 1 while
//!   battery saver (Windows 11's energy saver) is on. A system call that
//!   reads kernel state and touches no disk, cheap enough to ask before each
//!   backlog file.
//! - **macOS** (catch-up, not built here): `NSProcessInfo.processInfo
//!   .isLowPowerModeEnabled` (macOS 12 and later), already reachable through
//!   the `objc2-foundation` the tree links for Apple.
//! - **Linux** (not built here): power-profiles-daemon's `ActiveProfile`
//!   property over D-Bus (`net.hadess.PowerProfiles`, renamed
//!   `org.freedesktop.UPower.PowerProfiles`) reading `power-saver`, which
//!   needs a D-Bus client the tree does not have yet.
//!
//! Until then both answer "not saving", which is what the reader did before
//! this existed.

/// Whether the system asks applications to save energy now.
pub trait Power: Send {
    fn saving(&self) -> bool;
}

/// Never saving: the default, for every composition that is not the daemon.
pub struct NeverSaving;

impl Power for NeverSaving {
    fn saving(&self) -> bool {
        false
    }
}

/// The operating system's answer.
pub struct SystemPower;

impl Power for SystemPower {
    fn saving(&self) -> bool {
        saver_on()
    }
}

#[cfg(windows)]
fn saver_on() -> bool {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // SAFETY: one call that fills a plain struct this frame owns.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    let answered = unsafe { GetSystemPowerStatus(&mut status) } != 0;
    answered && status.SystemStatusFlag == 1
}

#[cfg(not(windows))]
fn saver_on() -> bool {
    false
}
