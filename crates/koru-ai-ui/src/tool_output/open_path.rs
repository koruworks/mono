// SPDX-License-Identifier: MPL-2.0

//! **A changed file's path opens the file** (koru-ai D62, option *d*, the
//! user's call 2026-11-03).
//!
//! The path heading a diff in a call's detail is a link. A press opens the
//! real file when it is a type that opens in an editor
//! ([`OPENS_IN_EDITOR`]), and otherwise shows it selected in its folder in
//! the file manager — through the host, which owns that seam
//! (`koru_workspace_host::reveal`, as Runs' *Open location* does); a host
//! that has none opens the folder. A relative path is the agent's, so it
//! is read against the chat's working folder, which is the person's home
//! folder until one is chosen. A path no longer on the disk shows the
//! nearest folder above it that is.

use std::path::{Path, PathBuf};

/// **The extensions a press opens, and the only ones.** Opening hands the
/// file to the desktop with its default verb (`koru_window::open_uri`),
/// which is what a double-click does — and on Windows a double-click *runs*
/// a `.bat`, `.cmd`, `.ps1`, `.js`, `.vbs`, `.wsf`, `.hta`, `.py` or `.sh`,
/// and starts an `.exe`, `.com`, `.msi`, `.scr` or `.lnk`: exactly the
/// files an agent writes, and named here by a model's output. So this is
/// an allow-list of plain text and source that opens in an editor or a
/// viewer and runs nothing. Anything else — an unknown extension, or none
/// — is shown in its folder instead, where opening it is the person's own
/// double-click. The chat's attachment opener keeps the same rule for the
/// same reason: it writes source out as a `.txt` copy (chat-attachments
/// D12, D16). A web page (`.html`, `.svg`) is left out too: it opens in a
/// browser and runs its scripts there.
#[rustfmt::skip]
pub(crate) const OPENS_IN_EDITOR: &[&str] = &[
    // Prose, notes and data.
    "md", "markdown", "txt", "log", "json", "jsonc", "toml", "yaml", "yml", "ini", "cfg", "conf",
    "xml", "diff", "patch",
    // Source that a double-click edits rather than runs.
    "rs", "ts", "tsx", "css", "scss", "c", "h", "cc", "cpp", "hpp", "cs", "go", "java", "kt",
    "swift", "proto", "graphql",
];

/// Whether a press on `path` opens the file: its extension, in any case, is
/// on [`OPENS_IN_EDITOR`].
pub(crate) fn opens_in_editor(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            OPENS_IN_EDITOR
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// `path` as the agent meant it: as it is when absolute, else under the
/// chat's working `folder`, else under `home`. `None` when it is relative
/// and there is neither.
pub(crate) fn resolve(path: &str, folder: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    let path = Path::new(path.trim());
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    match folder {
        Some(folder) => Some(Path::new(folder).join(path)),
        None => home.map(|home| home.join(path)),
    }
}

/// What a press on a changed file's path does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PathAction {
    /// The file, with the desktop's default app.
    Open(PathBuf),
    /// Show this in the file manager: the file selected in its folder, or
    /// — for a path no longer there — the nearest folder that is.
    Reveal(PathBuf),
    /// Nothing on the disk to show.
    Nothing,
}

/// What a press on the resolved `path` does, `exists` answering for the
/// disk: an existing file of a type that opens in an editor opens; any
/// other existing path is shown in its folder; a missing one shows the
/// nearest folder above it that exists.
pub(crate) fn action(path: &Path, exists: impl Fn(&Path) -> bool) -> PathAction {
    if exists(path) {
        return if opens_in_editor(path) {
            PathAction::Open(path.to_path_buf())
        } else {
            PathAction::Reveal(path.to_path_buf())
        };
    }
    path.ancestors()
        .skip(1)
        .find(|folder| !folder.as_os_str().is_empty() && exists(folder))
        .map_or(PathAction::Nothing, |folder| {
            PathAction::Reveal(folder.to_path_buf())
        })
}

impl crate::ChatSurface {
    /// **Say the host can show a path in the file manager** — Koru's window
    /// can (`koru_workspace_host::reveal`), where the platform supports it.
    /// A surface nobody told opens the folder instead.
    pub fn offer_reveal_paths(&mut self, offered: bool) {
        self.reveal_paths_offered = offered;
    }

    /// A changed file's path was pressed for a file that is not opened
    /// directly: the host shows this in the file manager.
    pub fn take_reveal_path_request(&mut self) -> Option<PathBuf> {
        self.reveal_path_request.take()
    }

    /// **A press on a changed file's path** (D62): the file opens, or is
    /// shown in its folder ([`action`]).
    pub(crate) fn open_changed_file(&mut self, path: &str) -> bool {
        let folder = self
            .transcript
            .as_ref()
            .and_then(|transcript| transcript.chat.agent_working_folder.clone());
        let home = std::env::home_dir();
        let Some(path) = resolve(path, folder.as_deref(), home.as_deref()) else {
            return false;
        };
        match action(&path, |candidate| candidate.exists()) {
            PathAction::Open(file) => open_on_the_desktop(file),
            PathAction::Reveal(target) if self.reveal_paths_offered => {
                self.reveal_path_request = Some(target);
            }
            // No file manager seam: the folder itself, which a double-click
            // opens and nothing runs.
            PathAction::Reveal(target) => {
                let folder = if target.is_dir() {
                    target
                } else {
                    target.parent().map(Path::to_path_buf).unwrap_or(target)
                };
                open_on_the_desktop(folder);
            }
            PathAction::Nothing => return false,
        }
        true
    }
}

/// Hand `path` to the desktop off the thread that draws, as a web link is
/// ([`crate::open_web_link`]): the desktop may take as long as it likes. The
/// path is the person's own and is not logged.
fn open_on_the_desktop(path: PathBuf) {
    if cfg!(any(test, feature = "no-client")) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("koru-ai-open-file".into())
        .spawn(move || {
            if let Err(error) = koru_window::open_uri(&path.to_string_lossy()) {
                tracing::warn!(
                    target: "koru_ai::ui",
                    "AI UI could not open a changed file: {error}"
                );
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(
            target: "koru_ai::ui",
            "AI UI could not start the thread that opens a changed file: {error}"
        );
    }
}

#[cfg(test)]
mod tests;
