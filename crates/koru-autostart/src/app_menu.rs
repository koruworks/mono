// SPDX-License-Identifier: MPL-2.0

//! **Koru in the app menu, on Linux**: one desktop entry in
//! `~/.local/share/applications/`, naming this executable by its full path.
//!
//! A tarball has no install step, so until now the package carried a
//! `koru.desktop` to copy by hand, which had to assume the folder it was
//! extracted to. Koru writes the entry itself instead, when the person asks
//! (Welcome, or Settings → System), from the path it is actually running at.
//! Nothing needs root and nothing is copied: GNOME and KDE watch that folder,
//! so Koru is in the app list, with its own icon in the dock, at once.
//!
//! **Self-updates keep it right.** An update replaces files inside the same
//! folder, so the paths stay true. A folder that moves leaves an entry naming
//! a file that is gone, and [`repair`] points it at the new place the next
//! time Koru starts.
//!
//! **One entry per channel**, by the file name: `koru.desktop` for stable,
//! which is also the name the hand-copied entry had, so this one replaces it;
//! `koru-nightly.desktop` and so on for the rest. A nightly beside a stable
//! gets its own line in the menu.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_variables))]

use std::io;
use std::path::{Path, PathBuf};

/// Whether this platform has an app menu Koru can add itself to.
pub const SUPPORTED: bool = cfg!(target_os = "linux");

/// The key under which the entry keeps the executable it names. [`repair`]
/// reads it: it is how an entry tells "the folder moved" from "fine".
const EXE_KEY: &str = "X-Koru-Exe";

/// Add Koru to the app menu, or bring its entry up to date, naming `exe`.
pub fn add(exe: &Path) -> io::Result<()> {
    if !SUPPORTED {
        return Err(unsupported());
    }
    let exe_text = exe
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the path is not UTF-8"))?;
    let path = entry_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let icon = icon_beside(exe);
    let text = entry(
        koru_channel::segment(),
        exe_text,
        icon.as_deref().and_then(Path::to_str),
    );
    std::fs::write(path, text)
}

/// Take Koru out of the app menu. Removing an entry that is not there
/// succeeds.
pub fn remove() -> io::Result<()> {
    if !SUPPORTED {
        return Err(unsupported());
    }
    crate::xdg::remove(&entry_path()?)
}

/// Whether this channel's entry is there now: the file's answer, read fresh,
/// as [`crate::is_set`] reads the Run key.
pub fn is_added() -> bool {
    SUPPORTED && entry_path().is_ok_and(|path| path.is_file())
}

/// Point an existing entry at `exe` when the one it names is gone. Writes
/// nothing when there is no entry: the person took it out, or never put it
/// in. `Ok(true)` when it rewrote the entry.
///
/// **Only a missing executable counts.** Two copies of one channel in two
/// folders would otherwise take the entry from each other at every start; it
/// stays with the copy it was added for until that copy is gone. An entry
/// without [`EXE_KEY`] is the hand-copied one, whose `$HOME` path cannot be
/// checked, and is rewritten.
pub fn repair(exe: &Path) -> io::Result<bool> {
    if !SUPPORTED {
        return Ok(false);
    }
    let text = match std::fs::read_to_string(entry_path()?) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !needs_repair(&text, |named| named.is_file()) {
        return Ok(false);
    }
    add(exe)?;
    Ok(true)
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "adding Koru to the app menu is supported on Linux",
    )
}

fn entry_path() -> io::Result<PathBuf> {
    Ok(crate::xdg::data_home()?
        .join("applications")
        .join(format!("{}.desktop", file_id(koru_channel::segment()))))
}

/// The logo the package carries beside the executables, if this is a package:
/// a build from source has none there, and its entry goes without an icon
/// rather than naming a file that does not exist.
fn icon_beside(exe: &Path) -> Option<PathBuf> {
    let icon = exe.parent()?.join("assets").join("logo.png");
    icon.is_file().then_some(icon)
}

/// `koru` for stable, `koru-<segment>` for every other channel.
pub(crate) fn file_id(segment: &str) -> String {
    if segment == "stable" {
        "koru".to_owned()
    } else {
        format!("koru-{segment}")
    }
}

/// `Koru`, `Koru Nightly`, `Koru Internal Debug`: the name in the menu, which
/// is all that tells two channels apart there.
pub(crate) fn display_name(segment: &str) -> String {
    if segment == "stable" {
        return "Koru".to_owned();
    }
    let words: Vec<String> = segment
        .split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut letters = word.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default()
        })
        .collect();
    format!("Koru {}", words.join(" "))
}

/// The entry's text.
///
/// `Icon` takes a full path, which the spec allows, so the icon needs no copy
/// that could go stale. `StartupWMClass=koru` is winit's X11 default, the
/// binary's name, so the dock groups Koru's windows under this entry.
pub(crate) fn entry(segment: &str, exe: &str, icon: Option<&str>) -> String {
    let icon = icon
        .map(|icon| format!("Icon={}\n", crate::xdg::escape(icon)))
        .unwrap_or_default();
    format!(
        "# Written by Koru. Settings → System → Show Koru in the app menu\n\
         # removes it.\n\
         [Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Exec={exec}\n\
         {icon}\
         Terminal=false\n\
         Categories=Office;\n\
         StartupWMClass=koru\n\
         {EXE_KEY}={exe_value}\n",
        name = crate::xdg::escape(&display_name(segment)),
        exec = crate::xdg::exec_argument(exe),
        exe_value = crate::xdg::escape(exe),
    )
}

/// Whether an entry with this text should be pointed at the running build:
/// it names no executable Koru can check, or one `exists` says is gone.
pub(crate) fn needs_repair(text: &str, exists: impl Fn(&Path) -> bool) -> bool {
    match crate::xdg::value_of(text, EXE_KEY) {
        Some(exe) => !exists(Path::new(&exe)),
        None => true,
    }
}
