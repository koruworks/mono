// SPDX-License-Identifier: MPL-2.0

//! **Saving an item's reading to a text file, to look at it** (OCR TECHNICAL
//! §9; asked for by the maintainer: "For me to debug, add an option to right
//! click and save .ocr to a file so I can review it").
//!
//! Offered in the extracted-text tab's toolbar, and on a picture in a page
//! **only when `KORU_READ_DEBUG=1`**. The export is available whatever the
//! reading's status — including none yet, which is worth seeing too. It reads
//! `GET /v1/readings/{workspace}/{hash}` and adds nothing underneath.
//!
//! What it writes is `<name>.ocr.txt`, UTF-8: a short header, then each
//! page's words under a `--- Page N ---` line. This module is the pure half —
//! the flag, the name and the text — so every shape is tested without a
//! dialog, a store or a disk; the shell (`app.rs`, `save_reading`) does the
//! fetch and the write off the event loop, as *Save as…* does.
//!
//! The OCR tab translates its toolbar label. The inline picture's debug-only
//! menu retains the diagnostic label below.

use koru_api_client::ReadingAnswer;

/// The menu row's label, wherever the row appears.
pub const LABEL: &str = "Save reading to file\u{2026}";

/// What the entry saves: a file under Files, or a picture in the page the
/// person right-clicked it in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReadingTarget {
    /// A file entry, by its document id. Its workspace and bytes are looked
    /// up as *Save as…* looks them up, so a cross-workspace entry works.
    File(koru_core::DocumentId),
    /// A picture, by its blob address, in the current workspace — where the
    /// document showing it lives, as for an image's *Copy*.
    Picture { hash: String },
}

/// **Whether the entry is offered at all**: `KORU_READ_DEBUG`, read once.
pub fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| koru_env::switch(koru_env::name::KORU_READ_DEBUG))
}

/// `<name>.ocr.txt` — the item's own name with the suffix added, never a
/// replaced extension, so `Lease.pdf` and `Lease.txt` save to different
/// files. A picture has no name of its own, so it takes its address's first
/// twelve hex digits.
pub fn file_name(target: &ReadingTarget, name: Option<&str>) -> String {
    let base = match (target, name.map(str::trim).filter(|n| !n.is_empty())) {
        (_, Some(name)) => name.to_owned(),
        (ReadingTarget::Picture { hash }, None) => {
            let digest = hash.split_once(':').map_or(hash.as_str(), |(_, d)| d);
            format!("picture-{}", digest.chars().take(12).collect::<String>())
        }
        (ReadingTarget::File(_), None) => "file".to_owned(),
    };
    format!("{base}.ocr.txt")
}

/// **The file's text.** A header of `key: value` lines — reader, generation,
/// status, reason, source, confidence, pages read — then each page under
/// `--- Page N ---`. `None` (nothing read of these bytes here yet) is still a
/// file, saying so: the entry is offered whatever the status.
pub fn text(hash: &str, answer: Option<&ReadingAnswer>) -> String {
    let dash = |value: Option<String>| value.unwrap_or_else(|| "-".to_owned());
    let mut out = String::new();
    out.push_str("Koru reading (KORU_READ_DEBUG)\n");
    out.push_str(&format!("blob: {hash}\n"));
    let Some(answer) = answer else {
        out.push_str("status: not read yet\n");
        return out;
    };
    let pages_read = answer
        .pages_done
        .unwrap_or_else(|| u32::try_from(answer.pages.len()).unwrap_or(u32::MAX));
    out.push_str(&format!("reader: {}\n", dash(answer.reader.clone())));
    out.push_str(&format!("generation: {}\n", answer.generation));
    out.push_str(&format!("status: {}\n", dash(answer.status.clone())));
    out.push_str(&format!("reason: {}\n", dash(answer.reason.clone())));
    out.push_str(&format!("source: {}\n", dash(answer.source.clone())));
    out.push_str(&format!(
        "confidence: {}\n",
        dash(answer.confidence.map(|c| c.to_string()))
    ));
    out.push_str(&format!("pages read: {pages_read}\n"));
    for page in &answer.pages {
        out.push_str(&format!("\n--- Page {} ---\n", page.page));
        out.push_str(&page.text);
        if !page.text.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests;
