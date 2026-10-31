// SPDX-License-Identifier: MPL-2.0

//! **The freedesktop.org pieces both Linux halves share**: where the files go,
//! and the desktop entry format's two escape layers.
//!
//! Pure string handling, compiled on every platform so the tests that pin the
//! escaping run on whichever machine runs them. Only Linux calls it.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::io;
use std::path::{Path, PathBuf};

/// The key under which an autostart entry keeps the command it was given,
/// read back by `command_of`: undoing `Exec`'s quoting would need a parser
/// for it.
pub(crate) const COMMAND_KEY: &str = "X-Koru-Command";

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub(crate) fn config_home() -> io::Result<PathBuf> {
    base("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME`, or `~/.local/share`.
pub(crate) fn data_home() -> io::Result<PathBuf> {
    base("XDG_DATA_HOME", ".local/share")
}

/// The spec ignores a relative value, so this does too.
fn base(variable: &str, fallback: &str) -> io::Result<PathBuf> {
    if let Some(value) = std::env::var_os(variable) {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME"))?;
    Ok(PathBuf::from(home).join(fallback))
}

/// Removing a file that is not there succeeds, as clearing an absent Run
/// value does.
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// One argument of an `Exec` line, always quoted.
///
/// **Two layers, in this order.** Inside the quotes `"`, `` ` ``, `$` and `\`
/// take a backslash (the `Exec` key's quoting rule); then every backslash is
/// doubled, because the value is a string and the file format unescapes
/// strings before the quoting rule is applied. A literal `%` is `%%`, or it
/// would be read as a field code.
pub(crate) fn exec_argument(argument: &str) -> String {
    let mut quoted = String::from('"');
    for c in argument.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    escape(&quoted)
}

/// A string value: backslashes and line breaks escaped, so one value stays one
/// line.
pub(crate) fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('s') => out.push(' '),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The value of `key` in the file's `[Desktop Entry]` group, unescaped.
pub(crate) fn value_of(text: &str, key: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with('[') {
            in_entry = line.trim_end() == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((name, value)) = line.split_once('=')
            && name.trim_end() == key
        {
            return Some(unescape(value.trim_start()));
        }
    }
    None
}

/// The autostart entry for `command`, under `value_name`.
///
/// `NoDisplay` keeps it out of the app list: it is the sign-in's, not a second
/// launcher.
pub(crate) fn autostart_entry(value_name: &str, command: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Exec=sh -c {exec}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n\
         {COMMAND_KEY}={command}\n",
        name = escape(value_name),
        exec = exec_argument(command),
        command = escape(command),
    )
}
