// SPDX-License-Identifier: MPL-2.0

//! **Whether a program starts when the user signs in.**
//!
//! One per-user registry value under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, and nothing else. No
//! scheduled task, no service, no shortcut in the Startup folder — those need
//! elevation, a manifest or a filesystem path that survives an update, and this
//! needs none of them.
//!
//! ## Why it is a crate
//!
//! Two binaries now offer the setting: Koru itself, and the Koru Store. The
//! code lived inside `koru-workspace-host`, which the store must not link — its `App`
//! cannot exist without a workspace open. Writing the registry handling a second
//! time was the alternative and lost for the usual reason: every line of it is a
//! trap that has already been paid for once, and two copies means the next fix
//! has to be found twice.
//!
//! ## Each program owns its own value name
//!
//! `Koru` and `Koru Store` are separate entries, so enabling one never disturbs
//! the other and disabling one never removes the other. The name is the caller's
//! to choose and is not derived from the executable: an exe that moves or is
//! renamed must still be able to find and clear the entry it wrote.
//!
//! ## …and its own channel
//!
//! That rule was one dimension short for as long as only one build was ever
//! installed. Five channels ship side by side and share nothing on disk, so a
//! nightly enabling *start at sign-in* was overwriting stable's `Koru` value
//! with the nightly path, and disabling it in either removed it for both. Nobody
//! would report that: it is a setting that quietly changed its mind, not a
//! crash. [`channel_entry`] is the fix, and it is a *naming* rule — nothing
//! about `set`, `clear` or `is_set` changed.
//!
//! ## Reading is as important as writing
//!
//! [`is_set`] exists because a checkbox that shows a *remembered* answer will
//! eventually disagree with the registry — somebody edits it, an installer
//! writes it, a different build clears it. The store has no preferences file at
//! all, so for it the registry is the only truth there is.
//!
//! ## macOS
//!
//! The same four verbs over a **LaunchAgent plist** in
//! `~/Library/LaunchAgents/` — one file per value name, `RunAtLoad`, the
//! command under `/bin/sh -c` so a full command line round-trips without this
//! crate learning to split words. Exactly the Run key's semantics: writing
//! the file changes what the *next* sign-in starts and starts nothing now,
//! and removing a file that is not there succeeds. `launchctl` is deliberately
//! not called — bootstrapping would also *run* the program immediately, which
//! `set` on Windows never did, and a second agent racing the live one is not a
//! behaviour to add by side effect.
//!
//! ## Linux
//!
//! The same four verbs over an **XDG autostart entry**, a `.desktop` file in
//! `~/.config/autostart/` named like the macOS plist. GNOME, KDE and every
//! other desktop that follows freedesktop.org start what is there at sign-in,
//! and, as on the other two, writing it starts nothing now. The command runs
//! under `sh -c` and rides a second time under `X-Koru-Command`, for the
//! macOS plist's reason.
//!
//! [`app_menu`] lives here too: it is the same file format, written under
//! the same rules, for the same running build.

#![cfg_attr(
    not(any(windows, target_os = "macos", target_os = "linux")),
    allow(unused_variables)
)]

use std::io;

pub mod app_menu;
mod xdg;

/// The key every entry lives under.
#[cfg(windows)]
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// Ask for `command` to run when this user signs in, under `value_name`.
///
/// `command` is a full command line, not a path — quote the executable
/// yourself, because Windows splits on spaces and `C:\Program Files\…` would
/// otherwise be read as a program called `C:\Program` with an argument.
pub fn set(value_name: &str, command: &str) -> io::Result<()> {
    platform::write(value_name, Some(command))
}

/// Remove the entry. **Removing one that is not there succeeds** — disabling
/// something already disabled is not a failure, and a caller that had to
/// distinguish the two would only ever ignore the difference.
pub fn clear(value_name: &str) -> io::Result<()> {
    platform::write(value_name, None)
}

/// Whether an entry exists under this name right now.
///
/// The *registry's* answer, read fresh. Not what was last written, and not what
/// a preferences file remembers.
pub fn is_set(value_name: &str) -> bool {
    platform::read(value_name).is_some()
}

/// The command an entry would run, if there is one.
///
/// For a caller that wants to know whether the entry points at *this* build —
/// an old install's entry naming a path that no longer exists is worse than no
/// entry, because the checkbox says yes and nothing starts.
pub fn command_of(value_name: &str) -> Option<String> {
    platform::read(value_name)
}

/// `base`, qualified with the channel this binary was built for.
///
/// `Koru`, `Koru (nightly)`, `Koru Store (internal-debug)` — one value name per
/// (program, channel) pair, which is the shape the rest of the product already
/// has: `%LOCALAPPDATA%\Koru\<segment>\` gives each channel its own everything,
/// and the Run key was the one place two installs still met.
///
/// **Stable keeps the bare name, and that is not tidiness.** Every stable build
/// shipped so far wrote `Koru`. Qualifying it to `Koru (stable)` would leave the
/// old value sitting in the registry under a name this build no longer looks at:
/// the checkbox would read *no* while something still started at sign-in, and
/// clearing it would clear the entry nobody is running. The bare name is the
/// compatible one, and stable is the channel entitled to it.
///
/// **The segment, not the channel.** [`koru_channel::segment`] tells
/// `internal-debug` from `internal-release`; `ReleaseChannel` calls both
/// `Internal`. Those are two different binaries at two different paths, and two
/// of them fighting over one value is the same bug this function exists to
/// remove — in miniature, on a developer's machine, where it would be blamed on
/// anything else.
///
/// Derived from the *running build*, never from a channel a caller was handed:
/// the entry has to be findable and clearable by the exe that wrote it, and that
/// exe knows only what it is.
pub fn channel_entry(base: &str) -> String {
    qualify(base, koru_channel::segment())
}

/// **The base name Koru's own startup entry lives under.**
///
/// Not derived from the executable, and deliberately not shared with the Koru
/// Store, which registers `Koru Store`: two programs writing one value name
/// would mean enabling either one silently disabled the other. Always passed
/// through [`channel_entry`] before it reaches the registry — five channels
/// install side by side.
///
/// It lives here rather than beside whichever host happens to write it,
/// because "who is entitled to this value name" is exactly the question this
/// crate exists to answer once. It was `koru-window`'s until the settings
/// authority took `start_on_login`, and a host that cannot link a window crate
/// now needs the same name.
pub const KORU_ENTRY: &str = "Koru";

/// **`--background` is Koru's own**: it starts into the tray rather than
/// opening a window over whatever the person is doing at sign-in.
pub const KORU_BACKGROUND_ARGUMENT: &str = "--background";

/// [`channel_entry`]'s value name for a build of `segment`, for the one caller
/// that is not that build: the Windows uninstaller `koru-release` writes, which
/// clears the entry the installed Agent may have set.
pub fn entry_for_segment(base: &str, segment: &str) -> String {
    qualify(base, segment)
}

/// The rule itself, with the segment passed in.
///
/// Split out so the tests can ask it about all five segments: a test of
/// [`channel_entry`] can only ever see the segment `cargo test` was built for,
/// which is `internal-debug`, and would therefore assert nothing about the one
/// case that matters — stable.
fn qualify(base: &str, segment: &str) -> String {
    if segment == "stable" {
        base.to_string()
    } else {
        format!("{base} ({segment})")
    }
}

/// The command line that starts `exe` with no arguments.
///
/// Quoted, because a path with a space in it is the normal case on Windows and
/// an unquoted one silently becomes several arguments.
pub fn quoted(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}

/// `Koru (beta)` → `do.koru.autostart.koru-beta`: launchd wants a
/// reverse-DNS label, XDG recommends a reverse-DNS file name, and the file has
/// to be findable by the exe that wrote it, so the mapping is deterministic
/// and total.
#[cfg_attr(windows, allow(dead_code))]
fn label(value_name: &str) -> String {
    let slug: String = value_name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!(
        "do.koru.autostart.{}",
        slug.split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    )
}

// --------------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod platform {
    use std::io;
    use std::path::PathBuf;

    pub fn write(value_name: &str, command: Option<&str>) -> io::Result<()> {
        let path = plist_path(value_name)?;
        match command {
            Some(command) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(path, plist(value_name, command))
            }
            None => match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
        }
    }

    pub fn read(value_name: &str) -> Option<String> {
        let text = std::fs::read_to_string(plist_path(value_name).ok()?).ok()?;
        extract_command(&text)
    }

    fn plist_path(value_name: &str) -> io::Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME"))?;
        Ok(PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", super::label(value_name))))
    }

    /// The command rides twice: as the `/bin/sh -c` argument launchd runs,
    /// and under `KoruCommand`, which is what [`read`] answers — so a full
    /// command line round-trips byte-for-byte without a plist parser.
    pub(super) fn plist(value_name: &str, command: &str) -> String {
        let label = super::label(value_name);
        let escaped = escape(command);
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>-c</string>
        <string>{escaped}</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KoruCommand</key><string>{escaped}</string>
</dict>
</plist>
"#
        )
    }

    pub(super) fn extract_command(text: &str) -> Option<String> {
        let after = text.split("<key>KoruCommand</key>").nth(1)?;
        let start = after.find("<string>")? + "<string>".len();
        let end = after.find("</string>")?;
        Some(unescape(&after[start..end]))
    }

    fn escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn unescape(value: &str) -> String {
        value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }
}

// ------------------------------------------------------------------- Windows

#[cfg(windows)]
mod platform {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW,
    };

    /// A NUL-terminated wide string. Windows needs the terminator and Rust's
    /// `OsStr` does not carry one, so it is appended here rather than at each
    /// of the four call sites.
    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }

    /// Set `value_name` to `command`, or delete it when `command` is `None`.
    pub fn write(value_name: &str, command: Option<&str>) -> io::Result<()> {
        let key = match command {
            Some(_) => open_or_create(KEY_SET_VALUE)?,
            None => match open(KEY_SET_VALUE) {
                Ok(key) => key,
                Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                    return Ok(());
                }
                Err(error) => return Err(error),
            },
        };
        let name = wide(value_name);
        let status = match command {
            Some(command) => {
                let bytes = wide(command);
                // SAFETY: `key` is open, `name` and `bytes` are NUL-terminated
                // and live across the call, and the length is in *bytes* — the
                // API takes bytes even though the data is UTF-16.
                unsafe {
                    RegSetValueExW(
                        key,
                        name.as_ptr(),
                        0,
                        REG_SZ,
                        bytes.as_ptr().cast(),
                        (bytes.len() * size_of::<u16>()) as u32,
                    )
                }
            }
            // SAFETY: as above; deleting takes no data.
            None => unsafe { RegDeleteValueW(key, name.as_ptr()) },
        };
        // Closed before the status is judged, so no path leaks the key.
        unsafe { RegCloseKey(key) };

        if status == ERROR_SUCCESS || (command.is_none() && status == ERROR_FILE_NOT_FOUND) {
            Ok(())
        } else {
            // The registry returns a `WIN32_ERROR`, which is unsigned; the
            // constructor wants the signed form.
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    /// The value's contents, or `None` for anything at all going wrong.
    ///
    /// Deliberately total: every failure — no key, no value, the wrong type,
    /// unreadable bytes — is the same answer to the only question being asked,
    /// which is "does this program start at login". A `Result` here would be a
    /// distinction every caller discards.
    pub fn read(value_name: &str) -> Option<String> {
        let key = open(KEY_QUERY_VALUE).ok()?;
        let name = wide(value_name);
        let mut kind = 0u32;
        let mut bytes = 0u32;

        // The documented two-call pattern: the first sizes the buffer.
        // SAFETY: `key` is open and `name` is NUL-terminated; null data and
        // length pointers are what ask for the size.
        let status = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                std::ptr::null_mut(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS || kind != REG_SZ || bytes == 0 {
            unsafe { RegCloseKey(key) };
            return None;
        }

        let mut buffer = vec![0u16; bytes as usize / size_of::<u16>() + 1];
        let mut length = bytes;
        // SAFETY: the buffer is at least `bytes` long, which is what the call
        // above said is needed.
        let status = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut kind,
                buffer.as_mut_ptr().cast(),
                &mut length,
            )
        };
        unsafe { RegCloseKey(key) };
        if status != ERROR_SUCCESS {
            return None;
        }

        // The stored length includes the terminator; `from_utf16_lossy` would
        // otherwise hand back a string ending in a NUL that compares unequal to
        // everything it looks identical to.
        let chars = length as usize / size_of::<u16>();
        let text = &buffer[..chars.min(buffer.len())];
        let text = match text.iter().position(|c| *c == 0) {
            Some(end) => &text[..end],
            None => text,
        };
        Some(String::from_utf16_lossy(text))
    }

    fn open(access: u32) -> io::Result<HKEY> {
        let key_name = wide(super::RUN_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: `key_name` is NUL-terminated and outlives the call; `key` is
        // written only on success.
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_name.as_ptr(), 0, access, &mut key) };
        if status == ERROR_SUCCESS {
            Ok(key)
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    /// Open the per-user Run key, creating it when this profile has never had
    /// a startup entry. Fresh and restricted Windows profiles are allowed not
    /// to carry the key yet; treating that as an installation failure made the
    /// first attempt to enable autostart impossible.
    fn open_or_create(access: u32) -> io::Result<HKEY> {
        let key_name = wide(super::RUN_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        let mut disposition = 0u32;
        // SAFETY: `key_name` is NUL-terminated and outlives the call; the class
        // and security pointers are optional, and both out-parameters are valid.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                key_name.as_ptr(),
                0,
                std::ptr::null_mut(),
                REG_OPTION_NON_VOLATILE,
                access,
                std::ptr::null(),
                &mut key,
                &mut disposition,
            )
        };
        if status == ERROR_SUCCESS {
            Ok(key)
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }
}

// --------------------------------------------------------------------- Linux

#[cfg(target_os = "linux")]
mod platform {
    use std::io;
    use std::path::PathBuf;

    pub fn write(value_name: &str, command: Option<&str>) -> io::Result<()> {
        let path = entry_path(value_name)?;
        match command {
            Some(command) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(path, super::xdg::autostart_entry(value_name, command))
            }
            None => super::xdg::remove(&path),
        }
    }

    pub fn read(value_name: &str) -> Option<String> {
        let text = std::fs::read_to_string(entry_path(value_name).ok()?).ok()?;
        super::xdg::value_of(&text, super::xdg::COMMAND_KEY)
    }

    fn entry_path(value_name: &str) -> io::Result<PathBuf> {
        Ok(super::xdg::config_home()?
            .join("autostart")
            .join(format!("{}.desktop", super::label(value_name))))
    }
}

// ---------------------------------------------------------------- everywhere else

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    use std::io;

    /// Not an error worth hiding. A caller offering the setting has to be able
    /// to say why it did not take, and `Unsupported` is the one answer that is
    /// true on every platform this is none of the three.
    pub fn write(_value_name: &str, _command: Option<&str>) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "starting at login is supported on Windows, macOS and Linux",
        ))
    }

    /// Never set, because nothing here can set it. A checkbox reading this gets
    /// `false`, which is the truth.
    pub fn read(_value_name: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests;
