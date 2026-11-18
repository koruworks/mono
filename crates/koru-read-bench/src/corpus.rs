// SPDX-License-Identifier: MPL-2.0

//! **What in a corpus is read, and as what.** By extension, because a corpus
//! is a folder of files a person named; the worker itself still decides by
//! the bytes.

use std::path::{Path, PathBuf};

/// What a file in the corpus is, for the report.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Pdf,
    Picture,
    Plain,
}

/// The kind and the MIME type the worker is told, or `None` for a file this
/// bench does not read.
pub fn kind_of(path: &Path) -> Option<(Kind, &'static str)> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "pdf" => (Kind::Pdf, "application/pdf"),
        "png" => (Kind::Picture, "image/png"),
        "jpg" | "jpeg" => (Kind::Picture, "image/jpeg"),
        "gif" => (Kind::Picture, "image/gif"),
        "webp" => (Kind::Picture, "image/webp"),
        "bmp" => (Kind::Picture, "image/bmp"),
        "tif" | "tiff" => (Kind::Picture, "image/tiff"),
        "txt" => (Kind::Plain, "text/plain"),
        "md" => (Kind::Plain, "text/markdown"),
        "csv" => (Kind::Plain, "text/csv"),
        "html" | "htm" => (Kind::Plain, "text/html"),
        _ => return None,
    })
}

/// Every readable file under `root`, recursively, in a stable order.
pub fn walk(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && kind_of(&path).is_some() {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests;
