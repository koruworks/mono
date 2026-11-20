// SPDX-License-Identifier: MPL-2.0

//! **The words a result row adds of its own.**
//!
//! Everything else on a row is the store's — the title, the location, the
//! snippet and its highlight ranges arrive on the hit and are drawn as they
//! came. Three things are this surface's to say, and they are here:
//!
//! - **the page** a file's words were read from (*Page 12*), drawn ahead of the
//!   snippet;
//! - **the picture mark** before the words read from a picture inside a
//!   document — a small muted glyph, then the picture's name only when a
//!   person gave it one ([`is_picture`], [`picture_name`]), drawn as its own
//!   runs ahead of the snippet exactly as the page is;
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

/// **Whether a hit's words were read from a picture inside a document** —
/// the hit that starts its snippet with the picture mark (OCR EXPERIENCE §3).
///
/// Decided from the store's tokens and nothing translated: the field says
/// the words were read ([`field::READ`](koru_store_wire::search::field::READ))
/// and the node they were read from is a picture (`image`, the stable kind
/// token). A file's words are read too, but a file is a result of its own
/// with its file icon, so it carries no mark.
pub(crate) fn is_picture(hit: &SearchHit) -> bool {
    hit.field == koru_store_wire::search::field::READ && hit.kind == PICTURE_KIND
}

/// The node kind of a picture, as documents store it (`Kind::Image`). Spelled
/// here because this crate links no node model, as `koru-search` spells it.
const PICTURE_KIND: &str = "image";

/// What follows the picture mark: the name a person gave the picture, or
/// `None` for a picture nobody named — which shows the mark alone. `None`
/// for every hit that is not a picture.
pub(crate) fn picture_name(hit: &SearchHit) -> Option<&str> {
    if !is_picture(hit) {
        return None;
    }
    hit.picture_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
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
    match list_path(koru_i18n::current(), hit) {
        Some(path) => path,
        None => context_line(&hit.breadcrumb, hit.lifecycle),
    }
}

/// Where a task list is: *Tasks / Top 3 / Backlog*, in the reader's language
/// and with its separator. `None` for a hit that is not a list.
///
/// Assembled here from the names the store sends, rather than read from the
/// store's own breadcrumb, because *Tasks* is this surface's word.
pub(crate) fn list_path(lang: Lang, hit: &SearchHit) -> Option<String> {
    let list = hit.list.as_ref()?;
    let mut parts = vec![t(lang, Key::SearchTasksCrumb)];
    parts.extend(list.path.iter().map(String::as_str));
    parts.push(hit.title.as_str());
    Some(parts.join(t(lang, Key::TaskListPathSeparator)))
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
    // **A picture says so in words**, not only with its mark (EXPERIENCE §3
    // "without a mouse"), so nobody hearing the row takes read words for
    // typed ones.
    if is_picture(hit) {
        return match picture_name(hit) {
            Some(name) => fill(
                t(lang, Key::SearchResultSpokenPictureNamed),
                &[&hit.title, &context, name, &hit.snippet],
            ),
            None => fill(
                t(lang, Key::SearchResultSpokenPicture),
                &[&hit.title, &context, &hit.snippet],
            ),
        };
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
