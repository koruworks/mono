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
//! **A Mac app opened from Finder has the opposite problem**: launchd gives
//! it `PATH=/usr/bin:/bin:/usr/sbin:/sbin` and nothing else. `/usr/local/bin`
//! comes from `path_helper` in `/etc/zprofile` and Homebrew's
//! `/opt/homebrew/bin` from the `brew shellenv` line in `~/.zprofile` — files
//! only a *login* shell reads — and the lines that put `~/.local/bin` and
//! `~/.opencode/bin` first usually sit in `~/.zshrc`, which only an
//! *interactive* one reads. Koru said "Not installed" about every agent but
//! Claude, and Claude's chats failed anyway:
//! `claude-agent-acp` is a `#!/usr/bin/env node` script, and the child it ran
//! in had no `node` on its `PATH`. So on macOS and Linux the search adds,
//! after the inherited entries, the directories the agents' own installers
//! use — for what only `~/.zshrc` adds — and the user's **login shell's**
//! `PATH`, captured once per process ([`login_shell_directories`]); see
//! [`extra_directories`] for the order.
//!
//! **Children get the same list.** Finding a program is half of it: a probe,
//! a chat's adapter, a terminal tab and an *Install* tab are all children
//! that look things up on `PATH` themselves (`env node`, an npm `.cmd` shim
//! calling `node`, a shell running `npm`), so every spawn site sets the
//! child's `PATH` to [`effective_path`] unless the launch names its own
//! (`with_effective_path`).
//!
//! The merging, de-duplication, `%VAR%` expansion and marker parsing are
//! pure functions over strings and bytes so they are tested on every
//! platform; only [`registry_paths`] and [`capture_login_path`] touch the
//! system.

// The pure half is compiled everywhere so its tests run on every platform;
// each platform calls only its own part.
#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The search directories for this process: the inherited `PATH`, then (on
/// Windows) the machine and user `Path` as the registry holds them now, or
/// (on macOS and Linux) the login shell's `PATH` and the agent installers'
/// directories — without repeats.
pub(crate) fn directories() -> Vec<PathBuf> {
    let inherited = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    #[cfg(windows)]
    return merge_windows(inherited, registry_directories());
    #[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
    return merge_unix(inherited, user_directories());
    #[cfg(not(any(windows, all(unix, not(any(target_os = "android", target_os = "ios"))))))]
    inherited
}

/// **The `PATH` a child of this process should run with**: [`directories`],
/// joined. Recomputed on every call, as the search is — the Windows registry
/// is read again and the installer directories are checked again, so a
/// program installed a moment ago is on it; only the login shell's answer is
/// cached. An entry the platform cannot write into a `PATH` (a `:` in a Unix
/// directory name) is left out rather than failing the whole value.
pub fn effective_path() -> OsString {
    let joinable = directories()
        .into_iter()
        .filter(|directory| std::env::join_paths([directory]).is_ok());
    std::env::join_paths(joinable).unwrap_or_default()
}

/// **Give `command` the [effective `PATH`](effective_path)** — the one
/// helper every spawn of an agent CLI, a probe or a session calls. Call it
/// before a launch's own environment is applied, so a `PATH` the launch sets
/// itself (a profile, a test fixture) still wins: `Command` keeps the last
/// value set for a name. With nothing to search at all it leaves the child
/// what it would have inherited.
///
/// On Unix this is also the `PATH` `Command` resolves a bare program name
/// through, so it covers the lookup of the program itself.
pub fn with_effective_path(command: &mut std::process::Command) -> &mut std::process::Command {
    let path = effective_path();
    if !path.is_empty() {
        command.env("PATH", path);
    }
    command
}

/// `inherited`, then each of `extra` that it does not already hold, in
/// order and once. A Unix directory with and without a trailing `/` is one
/// entry. Only absolute `extra` entries are taken: an empty or relative
/// entry in a shell's `PATH` means "the current directory", which a search
/// for an agent must never pick up from somebody else's configuration.
pub(crate) fn merge_unix(inherited: Vec<PathBuf>, extra: Vec<PathBuf>) -> Vec<PathBuf> {
    let key = |path: &PathBuf| {
        let text = path.to_string_lossy();
        let trimmed = text.trim_end_matches('/');
        if trimmed.is_empty() {
            text.into_owned()
        } else {
            trimmed.to_owned()
        }
    };
    let mut seen = std::collections::HashSet::new();
    inherited
        .into_iter()
        .chain(extra.into_iter().filter(|path| path.is_absolute()))
        .filter(|path| seen.insert(key(path)))
        .collect()
}

/// What a Mac or Linux process adds to its inherited `PATH`.
#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
fn user_directories() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    extra_directories(
        home.as_deref(),
        cfg!(target_os = "macos"),
        login_shell_directories(),
        Path::is_dir,
    )
}

/// **What is added after the inherited `PATH`, in order**: the agent
/// installers' own directories under `home`, then the login shell's `login`
/// entries, then — on a Mac — Homebrew's `bin`s. An installer directory is
/// taken only when `exists` says it is there, checked on every call, so
/// one created by an install a moment ago counts.
///
/// - **The installers' directories come first** because that is where a
///   person's terminal has them: each of these installers *prepends* its
///   directory in `~/.zshrc` or `~/.bashrc` (OpenCode's script writes
///   `export PATH=$INSTALL_DIR:$PATH`), which a login shell run with `-c`
///   does not read. Taking them after the login shell's would find an older
///   npm-installed copy in `/opt/homebrew/bin` before the one the person
///   runs.
///   - `~/.local/bin`: Claude Code's native installer, Codex's standalone
///     one, Muse's and Devin's (`known_locations` names Claude's and Devin's).
///   - `~/.opencode/bin`: OpenCode's `curl … | bash` installer, which takes
///     no override (its `install` script sets `INSTALL_DIR=$HOME/.opencode/bin`).
///   - `~/.grok/bin`: where Grok's installer links `grok`.
/// - **Homebrew's `bin`s come last**, as the answer for when the login shell
///   could not be asked: `/opt/homebrew/bin` (Apple silicon), then
///   `/usr/local/bin` (Intel, and nodejs.org's installer) — npm's global bin,
///   where `node` and every npm-installed agent is. A login shell that
///   answered already has them, and the duplicates are dropped.
///
/// Only these: a directory that is merely common (`~/.npm-global/bin`, a
/// version manager's) is a guess, and the login shell already knows a
/// person's own choices.
pub(crate) fn extra_directories(
    home: Option<&Path>,
    macos: bool,
    login: &[PathBuf],
    exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let installers = home
        .filter(|home| home.is_absolute())
        .into_iter()
        .flat_map(|home| {
            [[".local", "bin"], [".opencode", "bin"], [".grok", "bin"]].map(|segments| {
                segments
                    .iter()
                    .fold(home.to_path_buf(), |path, part| path.join(part))
            })
        });
    let homebrew = macos
        .then_some(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .into_iter()
        .flatten();
    installers
        .filter(|directory| exists(directory))
        .chain(login.iter().cloned())
        .chain(homebrew.filter(|directory| exists(directory)))
        .collect()
}

/// The text the login shell prints its `PATH` between. Whatever a profile
/// prints around it — a motd, a "last login", a version manager's notice —
/// is outside the two and ignored.
const PATH_BEGIN: &str = "__KORU_LOGIN_PATH_BEGIN__";
const PATH_END: &str = "__KORU_LOGIN_PATH_END__";

/// How long a login shell may take to say its `PATH`. Ordinary profiles
/// answer in a fraction of a second; a profile that waits on the network or
/// on a prompt is given up on rather than waited for, and the installer
/// directories still apply.
const LOGIN_SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The `PATH` value printed between the markers in `output`, or `None` when
/// either marker is missing — a shell that failed, timed out half-way or
/// printed nothing.
pub(crate) fn marked_path(output: &[u8]) -> Option<&[u8]> {
    let find = |haystack: &[u8], needle: &[u8]| {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    };
    let start = find(output, PATH_BEGIN.as_bytes())? + PATH_BEGIN.len();
    let length = find(&output[start..], PATH_END.as_bytes())?;
    Some(&output[start..start + length])
}

/// **The shell to ask**: `$SHELL` when it reads POSIX `-l -c` (the command
/// below is POSIX), otherwise the platform's default — zsh on a Mac, which is
/// where a Mac user's profile is, and `/bin/sh` elsewhere. fish and nushell
/// keep their `PATH` in a syntax this does not speak.
pub(crate) fn login_shell_program(shell: Option<&std::ffi::OsStr>, macos: bool) -> PathBuf {
    const POSIX: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];
    shell
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| POSIX.contains(&name))
        })
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(if macos { "/bin/zsh" } else { "/bin/sh" }))
}

/// The login shell's `PATH` entries, asked **once per process**: the files
/// that build it do not change while Koru runs, often enough to matter, and
/// a shell per probe would not be free. Empty when the shell could not be
/// asked. Never asked by this crate's own tests, which must not run a
/// person's profile; they inject directories instead.
#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
fn login_shell_directories() -> &'static [PathBuf] {
    static CAPTURED: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    CAPTURED.get_or_init(|| {
        if cfg!(test) {
            return Vec::new();
        }
        capture_login_path()
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default()
    })
}

/// Run the login shell once and read the `PATH` it prints between the
/// markers.
///
/// - **`-l`, never `-i`.** An interactive shell reads `~/.zshrc`, which is
///   where prompts, `compinit`'s "insecure directories?" question, tmux
///   auto-attach and the like live — things that wait on a terminal this
///   child does not have, and hang it. What only `~/.zshrc` adds is covered
///   by [`extra_directories`] instead.
/// - **No stdin, and a process group of its own**, so a profile that reads
///   the terminal Koru was started from gets `SIGTTIN` and stops, rather
///   than taking the person's typing; the deadline then ends it.
/// - **Read as it arrives, not to the end.** A profile may start a daemon
///   that keeps the pipe open after the shell exits; the answer is complete
///   at the closing marker, so that is when reading stops.
#[cfg(all(unix, not(any(target_os = "android", target_os = "ios"))))]
fn capture_login_path() -> Option<OsString> {
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::sync::mpsc::{self, RecvTimeoutError};

    let shell = login_shell_program(
        std::env::var_os("SHELL").as_deref(),
        cfg!(target_os = "macos"),
    );
    let mut child = Command::new(&shell)
        .arg("-l")
        .arg("-c")
        .arg(format!(
            "printf '%s%s%s' '{PATH_BEGIN}' \"$PATH\" '{PATH_END}'"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let finish = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let Some(mut stdout) = child.stdout.take() else {
        finish(&mut child);
        return None;
    };
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    let reader = std::thread::Builder::new()
        .name("koru-login-path".into())
        .spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stdout.read(&mut buffer) {
                if read == 0 || sender.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
    if reader.is_err() {
        finish(&mut child);
        return None;
    }
    let deadline = std::time::Instant::now() + LOGIN_SHELL_TIMEOUT;
    let mut output = Vec::new();
    let value = loop {
        if let Some(value) = marked_path(&output) {
            break Some(value.to_vec());
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(left) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Disconnected) => break marked_path(&output).map(<[u8]>::to_vec),
            Err(RecvTimeoutError::Timeout) => break None,
        }
    };
    finish(&mut child);
    value.map(|value| std::ffi::OsStr::from_bytes(&value).to_owned())
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
