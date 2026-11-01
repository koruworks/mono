// SPDX-License-Identifier: MPL-2.0

//! Open bytes Koru holds in another app, through a temporary copy.
//!
//! A workspace file in the Files view and a chat's attachment both live
//! somewhere no other app can read — the Store's blobs, the AI service's
//! folder — so opening one in the system's app means writing a copy the app
//! can open and handing that path to [`open_uri`](crate::open_uri). Both did
//! it; this is the one copy of how (chat-attachments D12), moved here from
//! the Files view so the chat, which does not link the workspace host, shares
//! it.
//!
//! **Slow: call it on a helper thread.** It writes a file and asks the shell,
//! and neither belongs on the thread that answers the person.
//!
//! **Text opens as text.** A copy opened [`OpenAs::Text`] is named
//! `<name>.txt` (unless it already ends in `.txt`), so the system opens it in
//! the text editor whatever the original's extension would have done: a `.js`
//! would otherwise run under Windows Script Host, and an `.html` open a
//! browser. The chat opens every text and source-code attachment that way
//! (D12, D16).

use std::path::PathBuf;

/// How the temporary copy is named, and so which app the system picks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OpenAs {
    /// Under its own name: the system picks the app its extension names.
    AsIs,
    /// Under `<name>.txt`: the system's text editor, whatever the name says.
    Text,
}

/// Where the copy of `name` is written: one stable folder per `key` (a file's
/// or an attachment's id), and a leaf cleaned of anything a path would read
/// as structure. Prefixing the leaf with `koru-` avoids Windows device names
/// (`CON`, `NUL`) while keeping the extension the system chooses an app by.
pub fn temporary_open_path(key: &str, name: &str, open_as: OpenAs) -> PathBuf {
    let leaf = name
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("file");
    let cleaned: String = leaf
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .take(180)
        .collect();
    let cleaned = if cleaned.trim_matches(['.', ' ']).is_empty() {
        "file"
    } else {
        cleaned.trim_end_matches(['.', ' '])
    };
    let already_text = cleaned
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("txt"));
    let leaf = match open_as {
        OpenAs::Text if !already_text => format!("koru-{cleaned}.txt"),
        OpenAs::AsIs | OpenAs::Text => format!("koru-{cleaned}"),
    };
    std::env::temp_dir().join("koru-open").join(key).join(leaf)
}

/// Write `bytes` to [`temporary_open_path`] and open the copy with the app
/// the system has for it. The path written, or why it could not be.
///
/// Success means the shell accepted the request, as with
/// [`open_uri`](crate::open_uri): a type with no app is the system's own
/// "how do you want to open this" dialog on Windows, and an error elsewhere.
pub fn open_bytes_as_file(
    key: &str,
    name: &str,
    bytes: &[u8],
    open_as: OpenAs,
) -> std::io::Result<PathBuf> {
    let destination = temporary_open_path(key, name, open_as);
    let parent = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("the temporary file has no parent"))?;
    std::fs::create_dir_all(parent)?;
    std::fs::write(&destination, bytes)?;
    crate::open_uri(destination.to_string_lossy().as_ref())?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_open_paths_are_leaf_only_and_keep_the_extension() {
        let path = temporary_open_path("abc", "..\\..\\CON:<report>?.pdf", OpenAs::AsIs);
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("koru-CON__report__.pdf")
        );
        assert!(path.ends_with("koru-open/abc/koru-CON__report__.pdf"));
    }

    /// Text and source code open as a `.txt` copy, so a click never runs a
    /// script or opens a browser.
    #[test]
    fn a_text_copy_is_named_txt_after_its_own_name() {
        for (name, expected) in [
            ("build.js", "koru-build.js.txt"),
            ("index.html", "koru-index.html.txt"),
            ("Makefile", "koru-Makefile.txt"),
            ("notes.txt", "koru-notes.txt"),
        ] {
            let path = temporary_open_path("id", name, OpenAs::Text);
            assert_eq!(
                path.file_name().and_then(|leaf| leaf.to_str()),
                Some(expected)
            );
        }
    }

    #[test]
    fn a_name_with_nothing_left_is_called_file() {
        let path = temporary_open_path("id", " .. ", OpenAs::AsIs);
        assert_eq!(
            path.file_name().and_then(|leaf| leaf.to_str()),
            Some("koru-file")
        );
    }
}
