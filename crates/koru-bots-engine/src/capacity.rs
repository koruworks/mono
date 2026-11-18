// SPDX-License-Identifier: MPL-2.0

//! Engine-owned admission over the durable inbox. Reserved and stopping
//! sessions occupy capacity until confirmed retirement.

mod admission;
pub use admission::{Admission, Decision, Reservation};
use uuid::Uuid;

pub const IDLE_WINDOW_MS: u64 = 5 * 60 * 1000;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SessionKey {
    pub bot: Uuid,
    pub thread: Uuid,
}

pub fn default_capacity(installed_bytes: u64) -> u32 {
    (installed_bytes / (2 * 1024 * 1024 * 1024)).clamp(1, 5) as u32
}

/// Called on a lifecycle thread at admission, never from dispatch or a timer.
/// Uses the same typed read-only authority adapter as delegated run roots.
pub fn configured(root: &std::path::Path) -> u32 {
    koru_settings::SettingsPaths::for_channel_root(
        root,
        koru_channel::channel_root().as_deref(),
        koru_channel::segment(),
    )
    .map(koru_settings::SettingsAuthority::new)
    .and_then(|authority| authority.global().bots_session_capacity)
    .filter(|limit| *limit > 0)
    .unwrap_or_else(|| default_capacity(installed_memory()))
}

#[cfg(windows)]
fn installed_memory() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetPhysicallyInstalledSystemMemory;
    let mut kilobytes = 0;
    // SAFETY: Windows writes one u64 through the valid out-pointer.
    if unsafe { GetPhysicallyInstalledSystemMemory(&mut kilobytes) } != 0 {
        kilobytes.saturating_mul(1024)
    } else {
        0
    }
}
#[cfg(not(windows))]
fn installed_memory() -> u64 {
    0
}

#[cfg(test)]
mod tests;
