// SPDX-License-Identifier: MPL-2.0

//! **A picture a call read, drawn from the disk** (koru-ai D38, the user's
//! call 2026-11-03).
//!
//! When an agent reads an image file — Claude's *Read* on a `.png`, Codex's
//! `view_image` — the call's detail shows the picture under its path. The
//! row keeps no picture: the agent's protocol sends it as `image` content,
//! which is not kept, and `agent_output` is text. What the row does keep is
//! the path, so the picture is read again from the file, by the same loader
//! a reply's linked picture goes through (`crate::ChatSurface::images`):
//! read and decoded off the thread that draws, then kept.
//!
//! [`super::file`] says *this read is of a picture* with a
//! [`Piece::Picture`] that has no size yet. The surface [`settle`]s it at
//! layout time: a relative path is the agent's, read against the chat's
//! working folder ([`super::open_path::resolve`], as a changed file's path
//! is); a picture already loaded gets its size and is drawn; one not asked
//! for yet is asked for; and one still loading, missing from the disk or
//! not decodable is left out, so the detail reads exactly as it did before
//! pictures — the path and whatever text the agent sent. No placeholder and
//! no error box: a read of a file since deleted is not a failure of the
//! call's.

use std::path::{Path, PathBuf};

use super::Piece;

/// **The extensions drawn as a picture**: what the reply's image decoder
/// reads (the `image` crate's default formats) and agents send as images.
pub(crate) const PICTURE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// A picture is never taller than a reply's picture box
/// (`crate::layout_cache::IMAGE_BOX_H`).
pub(crate) const MAX_HEIGHT: f32 = crate::layout_cache::IMAGE_BOX_H;

/// Whether the file at `path` is drawn as a picture: its extension, in any
/// case, is on [`PICTURE_EXTENSIONS`].
pub(crate) fn is_picture(path: &str) -> bool {
    Path::new(path.trim())
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            PICTURE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// How far a picture has come, as the surface's image cache knows it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Loaded {
    /// Decoded, this many pixels wide and high.
    Ready(u32, u32),
    /// Being read and decoded.
    Loading,
    /// Not on the disk, too large, or not a picture after all.
    Failed,
}

/// `pieces` with each picture settled: its path resolved by `resolve`, and
/// sized when `loaded` says it is ready. A picture never asked for is
/// pushed onto `wanted`; every picture not ready is left out.
pub(crate) fn settle(
    pieces: Vec<Piece>,
    resolve: impl Fn(&str) -> Option<PathBuf>,
    loaded: impl Fn(&str) -> Option<Loaded>,
    wanted: &mut Vec<PathBuf>,
) -> Vec<Piece> {
    pieces
        .into_iter()
        .filter_map(|piece| {
            let path = match piece {
                Piece::Picture { path, size: None } => path,
                other => return Some(other),
            };
            let resolved = resolve(&path)?;
            let key = resolved.to_string_lossy().into_owned();
            match loaded(&key) {
                Some(Loaded::Ready(width, height)) => Some(Piece::Picture {
                    path: key,
                    size: Some((width, height)),
                }),
                Some(Loaded::Loading | Loaded::Failed) => None,
                None => {
                    wanted.push(resolved);
                    None
                }
            }
        })
        .collect()
}

/// The size a `width` × `height` picture is drawn at in a column `column`
/// wide: as large as it is, made smaller to fit the column and
/// [`MAX_HEIGHT`], never larger — a 16-pixel icon stays an icon.
pub(crate) fn fitted(width: u32, height: u32, column: f32) -> (f32, f32) {
    let (width, height) = (width.max(1) as f32, height.max(1) as f32);
    let scale = (column.max(1.0) / width).min(MAX_HEIGHT / height).min(1.0);
    ((width * scale).max(1.0), (height * scale).max(1.0))
}

#[cfg(test)]
mod tests;
