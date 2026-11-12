// SPDX-License-Identifier: MPL-2.0

//! **The words a result row adds of its own.**
//!
//! Everything else on a row is the store's — the title, the location, the
//! snippet and its highlight ranges arrive on the hit and are drawn as they
//! came. Three things are this surface's to say, and they are here:
//!
//! - **the page** a file's words were read from (*Page 12*), drawn ahead of the
//!   snippet;
//! - **the context line**: where the result is, and whether it is in the bin;
//! - **the sentence a screen reader hears** for the row, which carries the page
//!   in words rather than leaving it to the picture of a number.
//!
//! A file's result does **not** name the page it is attached to. That line was
//! in the first sign-off and was withdrawn by the maintainer during execution:
//! which documents hold a file is a node graph, for another branch (OCR
//! QUESTIONS Q6, EXPERIENCE §7).
//!
//! Pure — a hit and a language in, strings out — so every row shape is tested
//! without a window, and the draw path asks the same functions the tests do.
//!
//! ## The page is never part of the snippet's text
//!
//! The snippet's highlight ranges are byte offsets into the snippet the store
//! sent, and the shared measured-run component elides *both* ends of a long
//! snippet around them. A prefix glued onto the front would either shift every
//! range or be the first thing elided away. So the page is its own run, drawn
//! before the snippet, and the snippet is drawn exactly as it came, ranges
//! untouched, in the width that is left. [`page_label`] is therefore a label,
//! not a prefix of anything.
//!
//! ## Arabic
//!
//! The row is not mirrored — no layout in Koru is yet, and that is the standing
//! policy (`koru_i18n`'s module note, DESIGN.md "Eight locales, and Arabic").
//! What this module guarantees is that the page label is a **whole label in its
//! own run**: *صفحة ١٢* reads right to left inside its slot with Arabic-Indic
//! digits, and the separator is a run of its own, so the bidi algorithm never
//! carries the dot to the far side of the label. The day rows mirror, the slots
//! move and these strings do not change.

use koru_api_client::SearchHit;
use koru_i18n::{Key, Lang, fill, number, t};
use koru_store_wire::search::Lifecycle;

/// What joins two pieces of one line: *Page 12 · …*, *Acme / Files · In
/// Trash*.
///
/// Punctuation the surface already joins its context line with, not a word —
/// the same mark in all eight languages, as it is in `SearchChordsAnywhere`.
pub(crate) const SEPARATOR: &str = " · ";

/// *Page 12* — the page a hit's words were read from — or `None` for a hit
/// that names no page.
///
/// Only a [`field::READ`](koru_store_wire::search::field::READ) hit on a file
/// of more than one page carries one; the store leaves `page` absent for a
/// picture, a one-page PDF, a plain-text file and every other field. The
/// surface draws exactly what the store said and decides nothing more.
pub(crate) fn page_label(lang: Lang, hit: &SearchHit) -> Option<String> {
    hit.page.map(|page| page_words(lang, page))
}

fn page_words(lang: Lang, page: u32) -> String {
    fill(
        t(lang, Key::SearchResultPage),
        &[&number::decimal(lang, u64::from(page))],
    )
}

/// A row's context line: where the result is, then whether it is in the bin.
///
/// *Acme / Files / Manual.pdf · In Trash*. The addition goes **after** the
/// location, so the store's breadcrumb ranges — offsets into the location —
/// stay valid without being moved.
pub(crate) fn context_line(breadcrumb: &str, lifecycle: Lifecycle) -> String {
    let mut line = breadcrumb.to_owned();
    if lifecycle == Lifecycle::Trashed {
        // Untranslated, as it was before this module: the surface's own labels
        // have not been through `koru-i18n` yet, and moving them is not this
        // change.
        line.push_str(SEPARATOR);
        line.push_str("In Trash");
    }
    line
}

/// [`context_line`] for a hit.
pub(crate) fn context(hit: &SearchHit) -> String {
    context_line(&hit.breadcrumb, hit.lifecycle)
}

/// The row as a screen reader says it: the title, where it is, and — for a
/// match inside — the words, with the page they were read from **in words**.
///
/// One sentence per language rather than pieces joined here, so each
/// language puts the page where it reads naturally and uses its own commas.
/// The snippet is the whole one the store sent, not the elided run on screen:
/// a reader has no width to fit.
pub(crate) fn spoken(lang: Lang, hit: &SearchHit, title_match: bool) -> String {
    let context = context(hit);
    if title_match {
        return fill(t(lang, Key::SearchResultSpoken), &[&hit.title, &context]);
    }
    match hit.page {
        Some(page) => fill(
            t(lang, Key::SearchResultSpokenOnPage),
            &[
                &hit.title,
                &context,
                &number::decimal(lang, u64::from(page)),
                &hit.snippet,
            ],
        ),
        None => fill(
            t(lang, Key::SearchResultSpokenExcerpt),
            &[&hit.title, &context, &hit.snippet],
        ),
    }
}

#[cfg(test)]
mod tests;
