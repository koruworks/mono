// SPDX-License-Identifier: MPL-2.0

//! **The directories a bare program name is looked for in.**
//!
//! A process's `PATH` is a copy taken when it started. On Windows that copy
//! goes stale: `koru-agent` is usually started at sign-in from the Run key and
//! lives for the whole session, and every window and session host it spawns
//! inherits its environment. An installer that adds a directory afterwards —
//! Claude Code's native installer puts `%USERPROFILE%\.local\bin` on the user
//! `Path`, npm puts `%APPDATA%\npm` there — writes it to the registry, and a
//! running process never sees it. Koru then said "Not installed" about a
//! program the person had just installed and could run from any new console.
//!
//! So on Windows the search also reads the **current** user and machine
//! `Path` from the registry, every time, after the inherited one: the
//! inherited entries keep their precedence (a launcher may have put something
//! first on purpose) and the registry only adds what the copy is missing.
//! Two registry reads cost microseconds, and nothing here runs on a timer.
//!
//! The merging, de-duplication and `%VAR%` expansion are pure functions over
//! strings so they are tested on every platform; only [`registry_paths`]
//! touches the system. Elsewhere the inherited `PATH` is the whole answer —
//! the missing directories of a Finder-launched Mac app are filled in by
//! `known_locations` instead.

// The pure half is compiled everywhere so its tests run on every platform;
// only Windows calls it.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::PathBuf;

/// The search directories for this process: the inherited `PATH`, then (on
/// Windows) the machine and user `Path` as the registry holds them now,
/// without repeats.
pub(crate) fn directories() -> Vec<PathBuf> {
    let inherited = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    #[cfg(windows)]
    return merge_windows(inherited, registry_directories());
    #[cfg(not(windows))]
    inherited
}

/// The registry's current `Path` entries, expanded through this process's
/// environment.
#[cfg(windows)]
fn registry_directories() -> Vec<PathBuf> {
    registry_paths()
        .into_iter()
        .flat_map(|value| {
            split_windows_path(&expand_windows_variables(&value, |name| {
                std::env::var(name).ok()
            }))
        })
        .collect()
}

/// Expand every `%NAME%` in `text` through `var`, the way
/// `ExpandEnvironmentStringsW` does: a name `var` does not know, and a lone
/// `%`, are left exactly as written. A `REG_EXPAND_SZ` `Path` routinely holds
/// `%USERPROFILE%` and `%SystemRoot%`.
///
/// `var` sees the name as written; on Windows `std::env::var` already matches
/// it without regard to case, as the system does.
pub(crate) fn expand_windows_variables(text: &str, var: impl Fn(&str) -> Option<String>) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(length) = after.find('%') else {
            // An unterminated `%` is text.
            expanded.push_str(&rest[start..]);
            return expanded;
        };
        let name = &after[..length];
        match (!name.is_empty()).then(|| var(name)).flatten() {
            Some(value) => {
                expanded.push_str(&value);
                rest = &after[length + 1..];
            }
            None => {
                // Unknown: keep the first `%` as text and look again from the
                // second, which may open a name that is known (`50%%PATH%`).
                expanded.push('%');
                expanded.push_str(name);
                rest = &after[length..];
            }
        }
    }
    expanded.push_str(rest);
    expanded
}

/// The entries of a Windows `Path` value: `;`-separated, with surrounding
/// double quotes removed (an entry may be quoted to carry a `;`, which
/// `std::env::split_paths` honours on Windows too) and empty entries dropped.
pub(crate) fn split_windows_path(value: &str) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in value.chars() {
        match character {
            '"' => quoted = !quoted,
            ';' if !quoted => entries.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    entries.push(current);
    entries
        .into_iter()
        .map(|entry| entry.trim().to_owned())
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// `inherited`, then each of `current` that it does not already hold, in
/// order and once. Windows compares directories without regard to case or a
/// trailing separator, so `C:\Tools\` and `c:\tools` are one entry.
pub(crate) fn merge_windows(inherited: Vec<PathBuf>, current: Vec<PathBuf>) -> Vec<PathBuf> {
    let key = |path: &PathBuf| {
        path.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    let mut seen = std::collections::HashSet::new();
    inherited
        .into_iter()
        .chain(current)
        .filter(|path| seen.insert(key(path)))
        .collect()
}

/// The machine `Path`, then the user's, as the registry holds them now —
/// unexpanded, because both are usually `REG_EXPAND_SZ`. This is the order
/// Windows itself composes a new process's `PATH` in. A value that is
/// missing or unreadable is simply absent: the inherited `PATH` still stands.
#[cfg(windows)]
pub(crate) fn registry_paths() -> Vec<String> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    [
        (
            HKEY_LOCAL_MACHINE,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
        ),
        (HKEY_CURRENT_USER, "Environment"),
    ]
    .into_iter()
    .filter_map(|(root, key)| platform::read_string(root, key, "Path"))
    .collect()
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }

    /// A `REG_SZ` or `REG_EXPAND_SZ` value as written — `RRF_NOEXPAND`, so the
    /// expansion is the tested [`super::expand_windows_variables`] rather than
    /// a second one here. `None` for any failure: there is nothing a caller
    /// would do differently with the reason.
    pub(super) fn read_string(root: HKEY, key: &str, value: &str) -> Option<String> {
        let key = wide(key);
        let value = wide(value);
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut bytes = 0u32;
        // The documented sizing call: null data asks for the length.
        // SAFETY: both names are NUL-terminated and outlive the call; a null
        // type and data pointer are allowed, and `bytes` is valid.
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS || bytes == 0 {
            return None;
        }
        // An installer may lengthen the value between the two calls; the
        // second then answers `ERROR_MORE_DATA` with the new length, so try a
        // few times rather than once.
        for _ in 0..3 {
            let mut buffer = vec![0u16; (bytes as usize).div_ceil(2)];
            let mut length = (buffer.len() * 2) as u32;
            // SAFETY: as above, and `buffer` holds `length` bytes.
            let status = unsafe {
                RegGetValueW(
                    root,
                    key.as_ptr(),
                    value.as_ptr(),
                    flags,
                    std::ptr::null_mut(),
                    buffer.as_mut_ptr().cast(),
                    &mut length,
                )
            };
            if status == ERROR_MORE_DATA {
                bytes = length;
                continue;
            }
            if status != ERROR_SUCCESS {
                return None;
            }
            // `RegGetValueW` terminates the string; the length counts the NUL.
            let units = &buffer[..(length as usize / 2).min(buffer.len())];
            let end = units
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(units.len());
            return Some(String::from_utf16_lossy(&units[..end]));
        }
        None
    }
}

#[cfg(test)]
mod tests;
